use crate::error::{PgError, Result, SqlState};

#[derive(Clone, Copy)]
pub(super) enum SettingType {
    TimeZone,
    Timeout,
    Isolation,
    SearchPath,
    ApplicationName,
    Encoding,
    Boolean,
    Integer {
        min: i64,
        max: i64,
        unit: MemoryUnit,
    },
    Real {
        min: f64,
    },
    PlanCacheMode,
    Text,
}

#[derive(Clone, Copy)]
pub(super) enum MemoryUnit {
    None,
    Kilobytes,
    Blocks,
}

#[derive(Clone, Copy)]
pub(super) enum SettingEffect {
    TimeZone,
    LockTimeout,
    StatementTimeout,
    Isolation,
    TransactionIsolation,
    SearchPath,
    Compatibility,
    Planner,
}

#[derive(Clone, Copy)]
pub(super) enum SettingContext {
    Session,
    Transaction,
    Backend,
    Server,
}

pub(super) enum SettingDefault {
    Text(&'static str),
    SearchPath(&'static [&'static str]),
    LockTimeout,
    Transaction,
}

pub(super) struct SettingSpec {
    pub(super) name: &'static str,
    pub(super) aliases: &'static [&'static str],
    pub(super) kind: SettingType,
    pub(super) default: SettingDefault,
    pub(super) effect: SettingEffect,
    pub(super) context: SettingContext,
}

impl SettingSpec {
    pub(super) fn validate_access(&self, strict: bool, writing: bool) -> Result<()> {
        if strict && matches!(self.effect, SettingEffect::Planner) {
            return Err(PgError::create(
                SqlState::FeatureNotSupported,
                format!("parameter {} is not modeled in strict mode", self.name),
            ));
        }
        if writing && !matches!(self.context, SettingContext::Session) {
            return Err(PgError::create(
                SqlState::CantChangeRuntimeParam,
                format!("parameter {} cannot be changed now", self.name),
            ));
        }
        Ok(())
    }
}

macro_rules! setting {
    ($name:literal, $kind:expr, $default:literal, $effect:ident) => {
        SettingSpec {
            name: $name,
            aliases: &[],
            kind: $kind,
            default: SettingDefault::Text($default),
            effect: SettingEffect::$effect,
            context: SettingContext::Session,
        }
    };
}

pub(super) fn list_settings() -> &'static [SettingSpec] {
    use MemoryUnit::{Blocks, Kilobytes, None};
    use SettingType::*;
    &[
        SettingSpec {
            name: "TimeZone",
            aliases: &["time zone"],
            kind: TimeZone,
            default: SettingDefault::Text("UTC"),
            effect: SettingEffect::TimeZone,
            context: SettingContext::Session,
        },
        SettingSpec {
            name: "lock_timeout",
            aliases: &[],
            kind: Timeout,
            default: SettingDefault::LockTimeout,
            effect: SettingEffect::LockTimeout,
            context: SettingContext::Session,
        },
        setting!("statement_timeout", Timeout, "0", StatementTimeout),
        setting!(
            "default_transaction_isolation",
            Isolation,
            "read committed",
            Isolation
        ),
        SettingSpec {
            name: "transaction_isolation",
            aliases: &[],
            kind: Isolation,
            default: SettingDefault::Transaction,
            effect: SettingEffect::TransactionIsolation,
            context: SettingContext::Transaction,
        },
        SettingSpec {
            name: "search_path",
            aliases: &[],
            kind: SearchPath,
            default: SettingDefault::SearchPath(&["$user", "public"]),
            effect: SettingEffect::SearchPath,
            context: SettingContext::Session,
        },
        setting!("application_name", ApplicationName, "", Compatibility),
        setting!("client_encoding", Encoding, "UTF8", Compatibility),
        setting!(
            "work_mem",
            Integer {
                min: 64,
                max: i32::MAX as i64,
                unit: Kilobytes
            },
            "4096",
            Planner
        ),
        setting!(
            "effective_cache_size",
            Integer {
                min: 1,
                max: i32::MAX as i64,
                unit: Blocks
            },
            "524288",
            Planner
        ),
        setting!(
            "min_parallel_table_scan_size",
            Integer {
                min: 0,
                max: 715827882,
                unit: Blocks
            },
            "1024",
            Planner
        ),
        setting!(
            "min_parallel_index_scan_size",
            Integer {
                min: 0,
                max: 715827882,
                unit: Blocks
            },
            "64",
            Planner
        ),
        setting!("random_page_cost", Real { min: 0.0 }, "4", Planner),
        setting!("seq_page_cost", Real { min: 0.0 }, "1", Planner),
        setting!("cpu_tuple_cost", Real { min: 0.0 }, "0.01", Planner),
        setting!("cpu_index_tuple_cost", Real { min: 0.0 }, "0.005", Planner),
        setting!("cpu_operator_cost", Real { min: 0.0 }, "0.0025", Planner),
        setting!("parallel_setup_cost", Real { min: 0.0 }, "1000", Planner),
        setting!("parallel_tuple_cost", Real { min: 0.0 }, "0.1", Planner),
        setting!(
            "join_collapse_limit",
            Integer {
                min: 1,
                max: i32::MAX as i64,
                unit: None
            },
            "8",
            Planner
        ),
        setting!(
            "from_collapse_limit",
            Integer {
                min: 1,
                max: i32::MAX as i64,
                unit: None
            },
            "8",
            Planner
        ),
        setting!("plan_cache_mode", PlanCacheMode, "auto", Planner),
        setting!("geqo", Boolean, "on", Planner),
        setting!("enable_async_append", Boolean, "on", Planner),
        setting!("enable_bitmapscan", Boolean, "on", Planner),
        setting!("enable_distinct_reordering", Boolean, "on", Planner),
        setting!("enable_gathermerge", Boolean, "on", Planner),
        setting!("enable_group_by_reordering", Boolean, "on", Planner),
        setting!("enable_hashagg", Boolean, "on", Planner),
        setting!("enable_hashjoin", Boolean, "on", Planner),
        setting!("enable_incremental_sort", Boolean, "on", Planner),
        setting!("enable_indexonlyscan", Boolean, "on", Planner),
        setting!("enable_indexscan", Boolean, "on", Planner),
        setting!("enable_material", Boolean, "on", Planner),
        setting!("enable_memoize", Boolean, "on", Planner),
        setting!("enable_mergejoin", Boolean, "on", Planner),
        setting!("enable_nestloop", Boolean, "on", Planner),
        setting!("enable_parallel_append", Boolean, "on", Planner),
        setting!("enable_parallel_hash", Boolean, "on", Planner),
        setting!("enable_partition_pruning", Boolean, "on", Planner),
        setting!("enable_partitionwise_aggregate", Boolean, "off", Planner),
        setting!("enable_partitionwise_join", Boolean, "off", Planner),
        setting!("enable_presorted_aggregate", Boolean, "on", Planner),
        setting!("enable_self_join_elimination", Boolean, "on", Planner),
        setting!("enable_seqscan", Boolean, "on", Planner),
        setting!("enable_sort", Boolean, "on", Planner),
        setting!("enable_tidscan", Boolean, "on", Planner),
        setting!("jit_above_cost", Real { min: -1.0 }, "100000", Planner),
        setting!(
            "jit_inline_above_cost",
            Real { min: -1.0 },
            "500000",
            Planner
        ),
        setting!(
            "jit_optimize_above_cost",
            Real { min: -1.0 },
            "500000",
            Planner
        ),
        setting!("jit_dump_bitcode", Boolean, "off", Planner),
        setting!("jit_expressions", Boolean, "on", Planner),
        setting!("jit_tuple_deforming", Boolean, "on", Planner),
        SettingSpec {
            name: "jit_debugging_support",
            aliases: &[],
            kind: Boolean,
            default: SettingDefault::Text("off"),
            effect: SettingEffect::Planner,
            context: SettingContext::Backend,
        },
        SettingSpec {
            name: "jit_profiling_support",
            aliases: &[],
            kind: Boolean,
            default: SettingDefault::Text("off"),
            effect: SettingEffect::Planner,
            context: SettingContext::Backend,
        },
        SettingSpec {
            name: "jit_provider",
            aliases: &[],
            kind: Text,
            default: SettingDefault::Text("llvmjit"),
            effect: SettingEffect::Planner,
            context: SettingContext::Server,
        },
    ]
}

pub(super) fn resolve_setting(name: &str) -> Result<&'static SettingSpec> {
    list_settings()
        .iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedObject,
                format!("unrecognized configuration parameter {name:?}"),
            )
        })
}
