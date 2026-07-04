#[cfg(test)]
mod tests {
    #[test]
    fn json_floats_round_trip() {
        let value: serde_json::Value = serde_json::from_str("0.11900000000000001").expect("parse");
        assert_eq!(
            serde_json::to_string(&value).expect("print"),
            "0.11900000000000001"
        );
    }
}
