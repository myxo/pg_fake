#![no_main]

#[path = "../../crates/pg_fake_sqlx/tests/property_tests.rs"]
mod property_tests;

chaos_theory::fuzz_target_libfuzzer!(property_tests::fuzz_generated_sql_matches_postgres);
