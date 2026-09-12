use crate::{
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::StatementContext,
    value::Value,
};

pub(super) fn evaluate_regex(value: &str, pattern: &str, flags: &str) -> Result<Value> {
    let mut insensitive = false;
    for flag in flags.chars() {
        match flag {
            'i' => insensitive = true,
            'c' => insensitive = false,
            's' => {}
            'b' | 'e' | 'm' | 'n' | 'p' | 'q' | 't' | 'w' | 'x' => {
                return reject_unsupported("regular expression flag is not implemented");
            }
            _ => {
                return Err(PgError::create(
                    SqlState::InvalidParameterValue,
                    "invalid regular expression option",
                ));
            }
        }
    }
    if !pattern.is_ascii() || (insensitive && !value.is_ascii()) {
        return reject_unsupported("non-ASCII regular expression matching is not implemented");
    }
    let mut escaped = false;
    let mut in_class = false;
    let mut previous = None;
    let mut class_range_pending = false;
    let mut class_range_ended = false;
    let mut class_count = 0;
    let mut quantified = false;
    let mut operand = false;
    let mut normalized = String::new();
    let mut characters = pattern.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            if character.is_ascii_alphanumeric() {
                return reject_unsupported("regular expression escape is not implemented");
            }
            if in_class {
                class_count += 1;
                class_range_ended = class_range_pending;
                class_range_pending = false;
            }
            escaped = false;
            operand = true;
            quantified = false;
        } else if character == '\\' {
            escaped = true;
        } else if in_class {
            let closes_class = character == ']' && class_count > 0;
            if character == '-' && class_count > 0 && characters.peek() != Some(&']') {
                if class_range_ended {
                    return Err(PgError::create(
                        SqlState::InvalidRegularExpression,
                        "invalid character range",
                    ));
                }
                class_range_pending = true;
            } else if class_range_pending {
                class_range_ended = true;
                class_range_pending = false;
            } else {
                class_range_ended = false;
            }
            if character != '^' || previous != Some('[') {
                class_count += 1;
            }
            if character == '['
                || (previous == Some(character) && matches!(character, '&' | '-' | '~'))
            {
                return reject_unsupported("regular expression character class is not implemented");
            }
            if closes_class {
                in_class = false;
                operand = true;
                quantified = false;
            }
        } else {
            match character {
                '[' => {
                    in_class = true;
                    class_count = 0;
                    class_range_pending = false;
                    class_range_ended = false;
                }
                '?' if previous == Some('(') || quantified => {
                    return reject_unsupported("regular expression construct is not implemented");
                }
                '*' | '+' | '?' => {
                    if !operand || quantified {
                        return Err(PgError::create(
                            SqlState::InvalidRegularExpression,
                            "quantifier operand invalid",
                        ));
                    }
                    quantified = true;
                }
                '{' if characters.peek().is_some_and(char::is_ascii_digit) => {
                    if !operand || quantified {
                        return Err(PgError::create(
                            SqlState::InvalidRegularExpression,
                            "quantifier operand invalid",
                        ));
                    }
                    let start = normalized.len();
                    normalized.push(character);
                    let mut closed = false;
                    for digit in characters.by_ref() {
                        normalized.push(digit);
                        if digit == '}' {
                            closed = true;
                            break;
                        }
                        if !digit.is_ascii_digit() && digit != ',' {
                            break;
                        }
                    }
                    if !closed {
                        return Err(PgError::create(
                            SqlState::InvalidRegularExpression,
                            "braces are not balanced",
                        ));
                    }
                    for bound in normalized[start + 1..normalized.len() - 1]
                        .split(',')
                        .filter(|bound| !bound.is_empty())
                    {
                        if bound.parse::<u16>().map_or(true, |bound| bound > 255) {
                            return Err(PgError::create(
                                SqlState::InvalidRegularExpression,
                                "invalid repetition count",
                            ));
                        }
                    }
                    quantified = true;
                    previous = Some('}');
                    continue;
                }
                '{' => {
                    normalized.push('\\');
                    operand = true;
                    quantified = false;
                }
                '(' | '|' | '^' | '$' => {
                    operand = false;
                    quantified = false;
                }
                _ => {
                    operand = true;
                    quantified = false;
                }
            }
        }
        normalized.push(character);
        previous = Some(character);
    }
    let regex = regex::RegexBuilder::new(&normalized)
        .case_insensitive(insensitive)
        .dot_matches_new_line(true)
        .build()
        .map_err(|error| PgError::create(SqlState::InvalidRegularExpression, error.to_string()))?;
    Ok(Value::Bool(regex.is_match(value)))
}

#[derive(Clone, Copy)]
enum LikeToken {
    Literal(char),
    One,
    Many,
    InvalidEscape,
}

pub(super) fn evaluate_like(
    value: &str,
    pattern: &str,
    escape: &str,
    insensitive: bool,
    context: &StatementContext,
) -> Result<Value> {
    let mut escapes = escape.chars();
    let escape = escapes.next();
    if escapes.next().is_some() {
        return Err(PgError::create(
            SqlState::InvalidEscapeSequence,
            "invalid escape string",
        ));
    }
    if insensitive && (!value.is_ascii() || !pattern.is_ascii()) {
        return reject_unsupported("non-ASCII case-insensitive matching is not implemented");
    }
    let mut pattern_chars = pattern.chars();
    let mut tokens = Vec::new();
    while let Some(character) = pattern_chars.next() {
        tokens.push(if Some(character) == escape {
            pattern_chars
                .next()
                .map(LikeToken::Literal)
                .unwrap_or(LikeToken::InvalidEscape)
        } else {
            match character {
                '%' => LikeToken::Many,
                '_' => LikeToken::One,
                c => LikeToken::Literal(c),
            }
        });
    }
    let text = value.chars().collect::<Vec<_>>();
    let mut text_index = 0;
    let mut pattern_index = 0;
    let mut retry = None;
    while text_index < text.len() && pattern_index < tokens.len() {
        context.check_timeout()?;
        match tokens[pattern_index] {
            LikeToken::Many => {
                pattern_index += 1;
                while pattern_index < tokens.len() {
                    match tokens[pattern_index] {
                        LikeToken::Many => {}
                        LikeToken::One => {
                            if text_index == text.len() {
                                return Ok(Value::Bool(false));
                            }
                            text_index += 1;
                        }
                        _ => break,
                    }
                    pattern_index += 1;
                }
                if pattern_index == tokens.len() {
                    return Ok(Value::Bool(true));
                }
                if matches!(tokens[pattern_index], LikeToken::InvalidEscape) {
                    return Err(PgError::create(
                        SqlState::InvalidEscapeSequence,
                        "LIKE pattern must not end with escape character",
                    ));
                }
                retry = Some((text_index, pattern_index));
                continue;
            }
            LikeToken::One => {}
            LikeToken::Literal(character)
                if if insensitive {
                    character.eq_ignore_ascii_case(&text[text_index])
                } else {
                    character == text[text_index]
                } => {}
            LikeToken::InvalidEscape => {
                return Err(PgError::create(
                    SqlState::InvalidEscapeSequence,
                    "LIKE pattern must not end with escape character",
                ));
            }
            _ => {
                if let Some((start, pattern)) = retry {
                    text_index = start + 1;
                    pattern_index = pattern;
                    retry = Some((text_index, pattern));
                    continue;
                }
                return Ok(Value::Bool(false));
            }
        }
        text_index += 1;
        pattern_index += 1;
        if pattern_index == tokens.len()
            && text_index < text.len()
            && let Some((start, pattern)) = retry
        {
            text_index = start + 1;
            pattern_index = pattern;
            retry = Some((text_index, pattern));
        }
    }
    Ok(Value::Bool(
        text_index == text.len()
            && tokens[pattern_index..]
                .iter()
                .all(|token| matches!(token, LikeToken::Many)),
    ))
}
