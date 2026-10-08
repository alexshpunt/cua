//! Strict pattern dispatch shared with the real COM binding and hermetic owner tests.
use cua_driver_contract::{SemanticOperation, SemanticPattern};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub code: &'static str,
    pub attempted: bool,
    pub hresult: Option<i32>,
}
impl Failure {
    pub fn refusal(code: &'static str) -> Self {
        Self {
            code,
            attempted: false,
            hresult: None,
        }
    }
}

/// Preparation cannot send input. Once a pattern call starts there is no retry.
pub(crate) trait Control {
    fn enabled(&mut self) -> Result<bool, Failure>;
    fn prepare(&mut self, pattern: SemanticPattern, value: Option<&str>) -> Result<(), Failure>;
    fn apply(&mut self) -> Result<(), Failure>;
}

pub(crate) fn execute(
    control: &mut impl Control,
    operation: SemanticOperation,
    value: Option<&str>,
) -> Result<SemanticPattern, Failure> {
    if !control.enabled()? {
        return Err(Failure::refusal("semantic_element_disabled"));
    }
    let pattern = match operation {
        SemanticOperation::Invoke => SemanticPattern::Invoke,
        SemanticOperation::Select => SemanticPattern::SelectionItem,
        SemanticOperation::SetValue => SemanticPattern::Value,
    };
    let pattern = match control.prepare(pattern, value) {
        Ok(()) => pattern,
        Err(error)
            if pattern == SemanticPattern::Value
                && error.code == "semantic_pattern_unsupported" =>
        {
            control.prepare(SemanticPattern::RangeValue, value)?;
            SemanticPattern::RangeValue
        }
        Err(error) => return Err(error),
    };
    control.apply()?;
    Ok(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Target {
        disabled: bool,
        missing_value: bool,
        fail_apply: bool,
        prepare_error: Option<Failure>,
        prepared: Vec<SemanticPattern>,
        calls: usize,
    }
    impl Control for Target {
        fn enabled(&mut self) -> Result<bool, Failure> {
            Ok(!self.disabled)
        }
        fn prepare(&mut self, pattern: SemanticPattern, _: Option<&str>) -> Result<(), Failure> {
            self.prepared.push(pattern);
            if let Some(error) = &self.prepare_error {
                return Err(error.clone());
            }
            if self.missing_value && pattern == SemanticPattern::Value {
                Err(Failure::refusal("semantic_pattern_unsupported"))
            } else {
                Ok(())
            }
        }
        fn apply(&mut self) -> Result<(), Failure> {
            self.calls += 1;
            if self.fail_apply {
                Err(Failure {
                    code: "semantic_provider_failed",
                    attempted: true,
                    hresult: Some(-1),
                })
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn exact_invoke_and_selection_use_one_pattern_once() {
        for (operation, pattern) in [
            (SemanticOperation::Invoke, SemanticPattern::Invoke),
            (SemanticOperation::Select, SemanticPattern::SelectionItem),
        ] {
            let mut target = Target::default();
            assert_eq!(execute(&mut target, operation, None), Ok(pattern));
            assert_eq!(target.prepared, vec![pattern]);
            assert_eq!(target.calls, 1);
        }
    }
    #[test]
    fn provider_failure_is_unknown_and_never_retries_another_pattern() {
        let mut target = Target {
            fail_apply: true,
            ..Default::default()
        };
        let error = execute(&mut target, SemanticOperation::SetValue, Some("42")).unwrap_err();
        assert_eq!(error.code, "semantic_provider_failed");
        assert!(error.attempted);
        assert_eq!(target.prepared, vec![SemanticPattern::Value]);
        assert_eq!(target.calls, 1);
    }
    #[test]
    fn range_value_is_chosen_only_when_value_pattern_is_unavailable() {
        let mut target = Target {
            missing_value: true,
            ..Default::default()
        };
        assert_eq!(
            execute(&mut target, SemanticOperation::SetValue, Some("42")),
            Ok(SemanticPattern::RangeValue)
        );
        assert_eq!(
            target.prepared,
            vec![SemanticPattern::Value, SemanticPattern::RangeValue]
        );
        assert_eq!(target.calls, 1);
    }
    #[test]
    fn unavailable_provider_and_missing_patterns_refuse_before_any_application_call() {
        for operation in [
            SemanticOperation::Invoke,
            SemanticOperation::Select,
            SemanticOperation::SetValue,
        ] {
            for failure in [
                Failure::refusal("semantic_pattern_unsupported"),
                Failure {
                    code: "semantic_provider_unavailable",
                    attempted: false,
                    hresult: Some(-42),
                },
            ] {
                let mut target = Target {
                    prepare_error: Some(failure.clone()),
                    ..Default::default()
                };
                assert_eq!(
                    execute(&mut target, operation, Some("42")).unwrap_err(),
                    failure
                );
                assert_eq!(target.calls, 0);
                if failure.code == "semantic_provider_unavailable" {
                    assert_eq!(target.prepared.len(), 1);
                }
            }
        }
    }
    #[test]
    fn disabled_control_refuses_without_preparing_or_calling_a_pattern() {
        let mut target = Target {
            disabled: true,
            ..Default::default()
        };
        assert_eq!(
            execute(&mut target, SemanticOperation::Invoke, None).unwrap_err(),
            Failure::refusal("semantic_element_disabled")
        );
        assert!(target.prepared.is_empty());
        assert_eq!(target.calls, 0);
    }
}
