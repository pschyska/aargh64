use aargh64::platform_override::PlatformOverride;
use kube::CustomResourceExt;

fn main() {
    print!(
        "{}",
        serde_yaml::to_string(&PlatformOverride::crd()).unwrap()
    )
}
