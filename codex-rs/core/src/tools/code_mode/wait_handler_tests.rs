use pretty_assertions::assert_eq;
use serde_json::json;

use super::DEFAULT_WAIT_YIELD_TIME_MS;
use super::ExecWaitArgs;

#[test]
fn wait_args_enforce_minimum_yield_time() {
    for (input, expected_yield_time_ms) in [
        (json!({ "cell_id": "default" }), DEFAULT_WAIT_YIELD_TIME_MS),
        (
            json!({ "cell_id": "clamped", "yield_time_ms": 1_000 }),
            DEFAULT_WAIT_YIELD_TIME_MS,
        ),
        (
            json!({ "cell_id": "preserved", "yield_time_ms": 30_000 }),
            30_000,
        ),
    ] {
        let expected_cell_id = input["cell_id"]
            .as_str()
            .expect("test input should include a cell ID")
            .to_string();

        assert_eq!(
            serde_json::from_value::<ExecWaitArgs>(input)
                .expect("wait arguments should deserialize"),
            ExecWaitArgs {
                cell_id: expected_cell_id,
                yield_time_ms: expected_yield_time_ms,
                max_tokens: None,
                terminate: false,
            }
        );
    }
}
