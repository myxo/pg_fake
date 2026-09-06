use super::*;
use bigdecimal::BigDecimal;
use sqlparser::ast;

#[derive(Debug, Clone, Copy)]
enum AggregateKind {
    CountAll,
    Count,
    Sum(BaseType),
    Average(BaseType),
    Minimum,
    Maximum,
    BooleanAnd,
    BooleanOr,
}

#[derive(Debug, Clone)]
pub(super) struct AggregateDescriptor {
    kind: AggregateKind,
    pub(super) distinct: bool,
    argument_type: Option<BaseType>,
    result_type: BaseType,
}

pub(super) struct AggregateCall<'a> {
    pub(super) descriptor: AggregateDescriptor,
    argument: Option<&'a ast::Expr>,
    filter: Option<&'a ast::Expr>,
}

#[derive(Clone)]
pub(super) struct AggregateInput {
    pub(super) included: bool,
    pub(super) argument: Option<Value>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn is_aggregate_function(function: &ast::Function) -> bool {
    function
        .name
        .0
        .first()
        .and_then(ast::ObjectNamePart::as_ident)
        .is_some_and(|name| {
            function.name.0.len() == 1
                && matches!(
                    normalize_identifier(name).as_str(),
                    "count" | "sum" | "avg" | "min" | "max" | "bool_and" | "bool_or"
                )
        })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn infer_aggregate_return_type(
    function: &ast::Function,
    schema: RowScope<'_>,
) -> Result<Option<BaseType>> {
    if !is_aggregate_function(function) {
        return Ok(None);
    }
    parse_aggregate_call(function, schema).map(|call| Some(call.descriptor.result_type))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn parse_aggregate_call<'a>(
    function: &'a ast::Function,
    schema: RowScope<'_>,
) -> Result<AggregateCall<'a>> {
    let name = normalize_identifier(
        function.name.0[0]
            .as_ident()
            .expect("aggregate name is an identifier"),
    );
    let signature_error = || {
        PgError::create(
            SqlState::UndefinedFunction,
            format!("function {name} does not exist"),
        )
    };
    if function.uses_odbc_syntax
        || !matches!(function.parameters, ast::FunctionArguments::None)
        || function.null_treatment.is_some()
        || function.over.is_some()
        || !function.within_group.is_empty()
    {
        return reject_unsupported("aggregate feature is not implemented");
    }
    let ast::FunctionArguments::List(arguments) = &function.args else {
        return Err(signature_error());
    };
    if !arguments.clauses.is_empty() {
        return reject_unsupported("aggregate argument feature is not implemented");
    }
    if let Some(filter) = &function.filter {
        let data_type = infer_expression_type(filter, schema)?;
        if data_type != BaseType::Bool && !is_null_literal(filter) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "FILTER expression must be type boolean",
            ));
        }
    }
    let distinct = matches!(
        arguments.duplicate_treatment,
        Some(ast::DuplicateTreatment::Distinct)
    );
    if name == "count"
        && (arguments.args.is_empty()
            || matches!(
                arguments.args.as_slice(),
                [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Wildcard)]
            ))
    {
        if distinct {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "DISTINCT requires an aggregate argument",
            ));
        }
        return Ok(AggregateCall {
            descriptor: AggregateDescriptor {
                kind: AggregateKind::CountAll,
                distinct,
                argument_type: None,
                result_type: BaseType::Int8,
            },
            argument: None,
            filter: function.filter.as_deref(),
        });
    }
    let [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))] =
        arguments.args.as_slice()
    else {
        return Err(signature_error());
    };
    let argument_type = infer_expression_type(argument, schema)?;
    if distinct {
        validate_equality_type(argument_type)?;
    }
    let (kind, result_type) = match name.as_str() {
        "count" => (AggregateKind::Count, BaseType::Int8),
        "sum" => match argument_type {
            BaseType::Int2 | BaseType::Int4 => (AggregateKind::Sum(BaseType::Int8), BaseType::Int8),
            BaseType::Int8 => (AggregateKind::Sum(BaseType::Numeric), BaseType::Numeric),
            BaseType::Float4 | BaseType::Float8 | BaseType::Numeric | BaseType::Interval => {
                (AggregateKind::Sum(argument_type), argument_type)
            }
            _ => return Err(signature_error()),
        },
        "avg" => match argument_type {
            BaseType::Int2 | BaseType::Int4 | BaseType::Int8 | BaseType::Numeric => {
                (AggregateKind::Average(BaseType::Numeric), BaseType::Numeric)
            }
            BaseType::Float4 | BaseType::Float8 => {
                (AggregateKind::Average(BaseType::Float8), BaseType::Float8)
            }
            BaseType::Interval => (
                AggregateKind::Average(BaseType::Interval),
                BaseType::Interval,
            ),
            _ => return Err(signature_error()),
        },
        "min" | "max" => {
            let result_type = match argument_type {
                BaseType::Int2
                | BaseType::Int4
                | BaseType::Int8
                | BaseType::Float4
                | BaseType::Float8
                | BaseType::Numeric
                | BaseType::Text
                | BaseType::Bpchar
                | BaseType::Bytea
                | BaseType::Date
                | BaseType::Time
                | BaseType::Timestamp
                | BaseType::TimestampTz
                | BaseType::Interval => argument_type,
                BaseType::Varchar => BaseType::Text,
                _ => return Err(signature_error()),
            };
            (
                if name == "min" {
                    AggregateKind::Minimum
                } else {
                    AggregateKind::Maximum
                },
                result_type,
            )
        }
        "bool_and" => {
            if argument_type != BaseType::Bool && !is_null_literal(argument) {
                return Err(signature_error());
            }
            (AggregateKind::BooleanAnd, BaseType::Bool)
        }
        "bool_or" => {
            if argument_type != BaseType::Bool && !is_null_literal(argument) {
                return Err(signature_error());
            }
            (AggregateKind::BooleanOr, BaseType::Bool)
        }
        _ => unreachable!("aggregate name was checked"),
    };
    Ok(AggregateCall {
        descriptor: AggregateDescriptor {
            kind,
            distinct,
            argument_type: Some(argument_type),
            result_type,
        },
        argument: Some(argument),
        filter: function.filter.as_deref(),
    })
}

pub(super) fn prepare_aggregate_function_input<F>(
    call: &AggregateCall<'_>,
    mut evaluate_expression: F,
) -> Result<AggregateInput>
where
    F: FnMut(&ast::Expr) -> Result<Value>,
{
    if let Some(filter) = call.filter {
        match evaluate_expression(filter)? {
            Value::Bool(true) => {}
            Value::Bool(false) | Value::Null => {
                return Ok(AggregateInput {
                    included: false,
                    argument: None,
                });
            }
            _ => unreachable!("aggregate FILTER expression was type-checked"),
        }
    }
    Ok(AggregateInput {
        included: true,
        argument: call.argument.map(evaluate_expression).transpose()?,
    })
}

pub(super) struct AggregateState {
    inputs: Option<Vec<AggregateInput>>,
    count: usize,
    value: Option<Value>,
    error: Option<PgError>,
}

impl AggregateState {
    pub(super) fn create(call: &AggregateDescriptor) -> Self {
        Self {
            inputs: call.distinct.then(Vec::new),
            count: 0,
            value: None,
            error: None,
        }
    }

    pub(super) fn add_input(&mut self, call: &AggregateDescriptor, input: AggregateInput) {
        if let Some(inputs) = &mut self.inputs {
            inputs.push(input);
        } else if self.error.is_none() {
            self.error = self.accumulate_input(call, input).err();
        }
    }

    fn accumulate_input(
        &mut self,
        call: &AggregateDescriptor,
        input: AggregateInput,
    ) -> Result<()> {
        if !input.included {
            return Ok(());
        }
        if matches!(call.kind, AggregateKind::CountAll) {
            self.count += 1;
            return Ok(());
        }
        let value = input
            .argument
            .expect("included aggregate input has an argument");
        if value.is_null() {
            return Ok(());
        }
        self.count += 1;
        self.value = match call.kind {
            AggregateKind::Count => None,
            AggregateKind::Sum(accumulator) | AggregateKind::Average(accumulator) => {
                let value = coercion::coerce(
                    value,
                    call.argument_type
                        .expect("aggregate expression has an argument type"),
                    PgType::create(accumulator),
                    CastContext::Implicit,
                )?;
                Some(match self.value.take() {
                    None => value,
                    Some(current) if accumulator == BaseType::Interval => {
                        evaluate_temporal_arithmetic(&ast::BinaryOperator::Plus, current, value)?
                    }
                    Some(current) => {
                        evaluate_numeric_operator(&ast::BinaryOperator::Plus, current, value)?
                    }
                })
            }
            AggregateKind::Minimum | AggregateKind::Maximum => Some(match self.value.take() {
                None => value,
                Some(current) => {
                    let ordering = compare_values(&value, &current)?;
                    if matches!(call.kind, AggregateKind::Minimum) && ordering == Ordering::Less
                        || matches!(call.kind, AggregateKind::Maximum)
                            && ordering == Ordering::Greater
                    {
                        value
                    } else {
                        current
                    }
                }
            }),
            AggregateKind::BooleanAnd | AggregateKind::BooleanOr => {
                let Value::Bool(value) = value else {
                    unreachable!("boolean aggregate argument was type-checked")
                };
                Some(Value::Bool(match (call.kind, self.value.take()) {
                    (AggregateKind::BooleanAnd, Some(Value::Bool(current))) => current && value,
                    (AggregateKind::BooleanOr, Some(Value::Bool(current))) => current || value,
                    (_, None) => value,
                    _ => unreachable!("boolean aggregate accumulator contains a boolean"),
                }))
            }
            AggregateKind::CountAll => {
                unreachable!("count all returned before argument evaluation")
            }
        };
        Ok(())
    }

    pub(super) fn finish(mut self, call: &AggregateDescriptor) -> Result<(Value, BaseType)> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        if let Some(inputs) = self.inputs.take() {
            let mut values = Vec::with_capacity(inputs.len());
            for input in inputs.into_iter().filter(|input| input.included) {
                let value = input
                    .argument
                    .expect("included aggregate input has an argument");
                if !value.is_null() {
                    let duplicate = values.iter().try_fold(false, |duplicate, existing| {
                        Ok(duplicate || compare_values(existing, &value)? == Ordering::Equal)
                    })?;
                    if !duplicate {
                        values.push(value);
                    }
                }
            }
            for value in values {
                self.accumulate_input(
                    call,
                    AggregateInput {
                        included: true,
                        argument: Some(value),
                    },
                )?;
            }
        }
        let value = match (call.kind, self.value) {
            (AggregateKind::Count | AggregateKind::CountAll, _) => {
                Value::Int8(i64::try_from(self.count).expect("row count must fit in int8"))
            }
            (_, None) => Value::Null,
            (AggregateKind::Average(BaseType::Interval), Some(sum)) => {
                evaluate_temporal_arithmetic(
                    &ast::BinaryOperator::Divide,
                    sum,
                    Value::Float8(self.count as f64),
                )?
            }
            (AggregateKind::Average(BaseType::Numeric), Some(sum)) => evaluate_numeric_operator(
                &ast::BinaryOperator::Divide,
                sum,
                Value::Numeric(BigDecimal::from(
                    i64::try_from(self.count).expect("row count must fit in int8"),
                )),
            )?,
            (AggregateKind::Average(BaseType::Float8), Some(sum)) => {
                let average = evaluate_numeric_operator(
                    &ast::BinaryOperator::Divide,
                    sum,
                    Value::Float8(self.count as f64),
                )?;
                match average {
                    Value::Float8(value) if value == 0.0 => Value::Float8(0.0),
                    average => average,
                }
            }
            (AggregateKind::Average(_), _) => unreachable!("average accumulator type was checked"),
            (_, Some(value)) => value,
        };
        Ok((value, call.result_type))
    }
}
