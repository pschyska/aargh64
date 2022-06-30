use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Serialize, Deserialize, Default, Debug, PartialEq, Clone, JsonSchema)]
#[kube(
    group = "aargh64.akquinet.de",
    version = "v1",
    kind = "PlatformOverride",
    plural = "platformoverrides",
    struct = "PlatformOverride",
    shortname = "apo",
    namespaced
)]
#[kube(printcolumn = r#"{"name":"Platform", "jsonPath": ".spec.platform", "type": "string"}"#)]
pub struct PlatformOverrideSpec {
    pub platform: String,
}
