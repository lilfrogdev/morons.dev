use super::{ModelDataUse, ModelRetention, ModelTrainingUse};

/// Server-owned settings must supply this at every admitted dispatch boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DataUseRestrictions {
    pub block_training_use: bool,
    pub require_zero_retention: bool,
}
impl DataUseRestrictions {
    pub fn permits(self, data: ModelDataUse) -> bool {
        (!self.block_training_use || data.training == ModelTrainingUse::NotUsed)
            && (!self.require_zero_retention || data.retention == ModelRetention::None)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restrictions_are_independent_and_unknown_policy_never_passes_a_restriction() {
        for training in [
            ModelTrainingUse::NotUsed,
            ModelTrainingUse::MayUsePromptsAndCompletions,
            ModelTrainingUse::NotDocumented,
        ] {
            for retention in [
                ModelRetention::None,
                ModelRetention::UpToThirtyDays,
                ModelRetention::NotZeroDataRetention,
                ModelRetention::NotDocumented,
            ] {
                let data = ModelDataUse {
                    training,
                    retention,
                };
                for block_training_use in [false, true] {
                    for require_zero_retention in [false, true] {
                        let policy = DataUseRestrictions {
                            block_training_use,
                            require_zero_retention,
                        };
                        assert_eq!(
                            policy.permits(data),
                            (!block_training_use || training == ModelTrainingUse::NotUsed)
                                && (!require_zero_retention || retention == ModelRetention::None)
                        );
                    }
                }
                assert!(DataUseRestrictions::default().permits(data));
            }
        }
    }
}
