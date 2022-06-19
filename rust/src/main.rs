use std::convert::Infallible;
use std::error::Error;
use std::process;

use anyhow::anyhow;
use futures::future::join_all;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
    Resource, ResourceExt,
};
use oci_distribution::client::ClientConfig;
use oci_distribution::manifest::OciManifest;
use oci_distribution::Reference;
use tokio::{main, task::JoinError};
use tracing::*;
use warp::{reply, Filter, Reply};

#[main]
async fn main() {
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        process::exit(130);
    });
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

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
    if let Some(pod) = req.object {
        res = match mutate(res.clone(), &pod).await {
            Ok(res) => {
                info!(
                    "Patching Pod(name={:?}, generate_name={:?})",
                    pod.meta().name,
                    pod.meta().generate_name
                );
                res
            }
            Err(err) => {
                info!(
                    "Unable to patch Pod(name={:?}, generate_name={:?}), skipping: {}",
                    pod.meta().name,
                    pod.meta().generate_name,
                    err
                );
                res
            }
        };
    };
    Ok(reply::json(&res.into_review()))
}

async fn get_manifest(reference: Reference) -> anyhow::Result<OciManifest> {
    let mut client = oci_distribution::Client::new(ClientConfig::default());
    let (manifest, _) = client
        .pull_manifest(
            &reference,
            &oci_distribution::secrets::RegistryAuth::Anonymous,
        )
        .await?;
    Ok(manifest)
}

async fn mutate(res: AdmissionResponse, pod: &Pod) -> Result<AdmissionResponse, Box<dyn Error>> {
    let patches: Vec<_> = pod
        .spec
        .as_ref()
        .ok_or(anyhow!("No pod spec found"))?
        .containers
        .iter()
        .enumerate()
        .map(|(i, container)| {
            let image = container.image.clone();
            let aargh_spec = pod.annotations()["aargh64"].clone();
            tokio::spawn(async move {
                let (spec_os, spec_architecture) = aargh_spec
                    .split_once("/")
                    .ok_or(anyhow!("No valid aargh64 annotation found"))?;
                let reference: Reference = image.ok_or(anyhow!("No container image"))?.parse()?;
                let manifest = match get_manifest(reference.clone()).await? {
                    OciManifest::Image(_) => Err(anyhow!("Not an image index")),
                    OciManifest::ImageIndex(m) => Ok(m),
                }?;

                if let Some(matched_image) = manifest.manifests.iter().find(|m| {
                    let platform = m.platform.as_ref().expect("No platform");
                    debug!(
                        "digest: {} platform: {}/{} request {}/{}",
                        m.digest, platform.os, platform.architecture, spec_os, spec_architecture
                    );
                    platform.os == spec_os && platform.architecture == spec_architecture
                }) {
                    Ok(json_patch::PatchOperation::Replace(
                        json_patch::ReplaceOperation {
                            path: format!("/spec/containers/{}/image", i),
                            value: serde_json::Value::String(format!(
                                "{}/{}@{}",
                                reference.registry(),
                                reference.repository(),
                                matched_image.digest
                            )),
                        },
                    ))
                } else {
                    Err(anyhow!("Could not find a matching platform"))
                }
            })
        })
        .collect();
    let patches = join_all(patches)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, JoinError>>()?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    Ok(res.with_patch(json_patch::Patch(patches))?)
}
