use std::convert::Infallible;
use std::error::Error;

use anyhow::anyhow;
use k8s_openapi::api::core::v1::Pod;
use kube::core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview};
use oci_distribution::client::ClientConfig;
use oci_distribution::manifest::OciManifest;
use oci_distribution::Reference;
use tokio::main;
use tracing::*;
use warp::{reply, Filter, Reply};

#[main]
async fn main() {
    tracing_subscriber::fmt::init();

    let routes = warp::path("mutate")
        .and(warp::body::json())
        .and_then(mutate_handler)
        .with(warp::trace::request());

    warp::serve(warp::post().and(routes))
        .tls()
        .cert_path("admission-controller-tls.crt")
        .key_path("admission-controller-tls.key")
        .run(([0, 0, 0, 0], 8443))
        .await;
}

async fn mutate_handler(body: AdmissionReview<Pod>) -> Result<impl Reply, Infallible> {
    let req: AdmissionRequest<_> = match body.try_into() {
        Ok(req) => req,
        Err(err) => {
            error!("invalid request: {}", err.to_string());
            return Ok(reply::json(
                &AdmissionResponse::invalid(err.to_string()).into_review(),
            ));
        }
    };

    let mut res = AdmissionResponse::from(&req);
    if let Some(obj) = req.object {
        res = match mutate(res.clone(), &obj).await {
            Ok(res) => {
                info!("accepted: {:?}", req.operation);
                res
            }
            Err(err) => {
                warn!("denied: {:?}: {}", req.operation, err);
                res.deny(err.to_string())
            }
        };
    };
    Ok(reply::json(&res.into_review()))
}

async fn get_manifest(image: &str) -> anyhow::Result<OciManifest> {
    let reference: Reference = image.parse()?;

    let mut client = oci_distribution::Client::new(ClientConfig::default());
    let (manifest, _) = client
        .pull_manifest(
            &reference,
            &oci_distribution::secrets::RegistryAuth::Anonymous,
        )
        .await?;
    Ok(manifest)
}

async fn mutate(res: AdmissionResponse, obj: &Pod) -> Result<AdmissionResponse, Box<dyn Error>> {
    if let Some(annotations) = &obj.metadata.annotations {
        if let Some(_arch) = annotations.get("aargh64") {
            if let Some(spec) = &obj.spec {
                for container in &spec.containers {
                    if let Some(image) = &container.image {
                        let manifest = match get_manifest(image).await? {
                            OciManifest::Image(_) => Err(anyhow!("Not an image index")),
                            OciManifest::ImageIndex(m) => Ok(m),
                        }?;
                        for m in manifest.manifests {
                            let platform = m.platform.ok_or_else(|| anyhow!("No platform"))?;
                            println!("{}", platform);
                        }

                        let patch =
                            json_patch::PatchOperation::Replace(json_patch::ReplaceOperation {
                                path: "/spec/containers[0].image".into(),
                                value: serde_json::Value::String(image.into()),
                            });

                        return Ok(res.with_patch(json_patch::Patch(vec![patch]))?);
                    }
                }
            }
        }
    }

    Ok(res)
}
