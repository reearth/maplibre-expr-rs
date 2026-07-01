//! The evaluation context: zoom level plus the feature being styled.

use std::collections::BTreeMap;

use crate::value::Value;

/// The feature (and its properties) an expression is evaluated against.
#[derive(Debug, Clone, Default)]
pub struct Feature {
    pub id: Option<Value>,
    pub properties: BTreeMap<String, Value>,
    pub geometry_type: Option<String>,
}

/// Everything an expression can read while evaluating: global parameters such
/// as `zoom`, the current [`Feature`], and the shared global-state map read by
/// the `global-state` operator.
#[derive(Debug, Clone, Default)]
pub struct EvaluationContext {
    pub zoom: Option<f64>,
    pub feature: Feature,
    pub global_state: BTreeMap<String, Value>,
}

impl EvaluationContext {
    pub fn new() -> EvaluationContext {
        EvaluationContext::default()
    }

    pub fn with_zoom(mut self, zoom: f64) -> EvaluationContext {
        self.zoom = Some(zoom);
        self
    }

    pub fn with_feature(mut self, feature: Feature) -> EvaluationContext {
        self.feature = feature;
        self
    }

    pub fn with_global_state(mut self, state: BTreeMap<String, Value>) -> EvaluationContext {
        self.global_state = state;
        self
    }
}
