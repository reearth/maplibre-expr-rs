//! The evaluation context: zoom level plus the feature being styled.

use std::collections::BTreeMap;

use crate::value::Value;

/// The feature (and its properties) an expression is evaluated against.
#[derive(Debug, Clone, Default)]
pub struct Feature {
    pub id: Option<Value>,
    pub properties: BTreeMap<String, Value>,
    pub geometry_type: Option<String>,
    /// The interactive feature state read by the `feature-state` operator.
    pub state: BTreeMap<String, Value>,
    /// The feature geometry in global tile coordinates, grouped into rings /
    /// lines (used by the `within` operator).
    pub geometry: Vec<Vec<(f64, f64)>>,
}

/// Everything an expression can read while evaluating: global parameters such
/// as `zoom`, the current [`Feature`], and the shared global-state map read by
/// the `global-state` operator.
#[derive(Debug, Clone, Default)]
pub struct EvaluationContext {
    pub zoom: Option<f64>,
    pub feature: Feature,
    pub global_state: BTreeMap<String, Value>,
    /// Image names known to be available (used by the `image` operator).
    pub available_images: Vec<String>,
    /// The canonical tile `(z, x, y)` the feature belongs to, if any.
    pub canonical: Option<(u32, u32, u32)>,
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
