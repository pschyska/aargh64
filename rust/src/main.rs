use std::process;
use std::{convert::Infallible, error::Error};

use aargh64::PlatformOverride;

use anyhow::anyhow;
use futures::{executor::block_on, future::join_all};
use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::{
    api::ListParams,
    core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
    Api, Client, Resource, ResourceExt,
};
use lazy_static::lazy_static;
use oci_distribution::client::ClientConfig;
use oci_distribution::manifest::OciManifest;
use oci_distribution::Reference;
use tokio::{main, select, signal::unix::SignalKind};
use tokio::{signal::unix::signal, task::JoinError};
use tracing::{debug, error, info, warn};
use warp::{reply, Filter, Reply};

lazy_static! {
    static ref CLIENT: Client =
        block_on(async move { Client::try_default().await.expect("Unable to build client") });
    static ref API: Api<PlatformOverride> = Api::default_namespaced(CLIENT.clone());
}

#[main]
async fn main() {
    tokio::spawn(async move {
        let int = SignalKind::interrupt();
        let mut int_signal = signal(int).unwrap();
        let term = SignalKind::terminate();
        let mut term_signal = signal(term).unwrap();
        let sig = select! {
            _ = int_signal.recv() => int,
            _ = term_signal.recv() => term
        };
        process::exit(sig.as_raw_value() + 128);
    });
    tracing_subscriber::fmt::init();

    let secrets: Api<Secret> = Api::default_namespaced(CLIENT.clone());
    let tls = secrets
        .get("admission-controller-tls")
        .await
        .expect("Unable to find admission-controller-tls secret");

    let routes = warp::path("mutate")
        .and(warp::body::json())
        .and_then(mutate_handler)
        .with(warp::trace::request());
    warp::serve(warp::post().and(routes))
        .tls()
        .cert(tls.data.as_ref().unwrap()["tls.crt"].clone().0)
        .key(tls.data.as_ref().unwrap()["tls.key"].clone().0)
        .run(([0, 0, 0, 0], 8443)) // in-cluster
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
            Ok(res) => res,
            Err(err) => {
                info!(
                    "Could not patch Pod(name={:?}, generate_name={:?}), skipping: {}",
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
    info!(
        "Patching Pod(name={:?}, generate_name={:?})",
        pod.meta().name,
        pod.meta().generate_name
    );
    let lp = ListParams::default();
    let overrides = match pod.annotations().clone().get("aargh64") {
        Some(a) => vec![a.clone()],
        None => API
            .list(&lp)
            .await?
            .items
            .iter()
            .map(|po| po.spec.platform.clone())
            .collect(),
    };
    if overrides.len() == 0 {
        debug!("No platform override found.");
        return Ok(res);
    }
    if overrides.len() != 1 {
        return Err(anyhow!(
            "Expected 0 or 1 platform overrides, but found {} ({}).",
            overrides.len(),
            overrides.join(",")
        )
        .into());
    }

    let patches: Vec<_> = pod
        .spec
        .as_ref()
        .ok_or(anyhow!("No pod spec found"))?
        .containers
        .iter()
        .enumerate()
        .map(|(i, container)| {
            let image = container.image.clone();
            let spec = overrides[0].clone();
            tokio::spawn(async move {
                let (spec_os, spec_architecture) = spec
                    .split_once("/")
                    .ok_or(anyhow!("Could not parse platform {}", spec))?;

                let reference: Reference = image.ok_or(anyhow!("No container image"))?.parse()?;
                let manifest = match get_manifest(reference.clone()).await? {
                    OciManifest::Image(_) => Err(anyhow!("Not an image index")),
                    OciManifest::ImageIndex(m) => Ok(m),
                }?;

                if let Some(matched_image) = manifest.manifests.iter().find(|m| {
                    let platform = m.platform.as_ref().expect("No platform");
                    platform.os == spec_os && platform.architecture == spec_architecture
                }) {
                    let patched = format!(
                        "{}/{}@{}",
                        reference.registry(),
                        reference.repository(),
                        matched_image.digest
                    );
                    info!(
                        "Updating image {} to {} (os={}, architecture={})",
                        reference, patched, spec_os, spec_architecture
                    );
                    Ok(json_patch::PatchOperation::Replace(
                        json_patch::ReplaceOperation {
                            path: format!("/spec/containers/{}/image", i),
                            value: serde_json::Value::String(patched),
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
