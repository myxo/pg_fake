pub mod analyzer;
pub mod api;
pub mod catalog;
pub mod error;
pub mod executor;
pub mod parser;
pub mod storage;
pub mod txn;
pub mod value;

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        assert_eq!(2 + 2, 4);
    }
}
