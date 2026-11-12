//! A device family's own RPC section (`DeviceFamily::routes`), reached as
//! a lane on a client's one connection to the engine (studio#420): the
//! contract every family with a section of its own sits on.
//!
//! A test family adds a `ProbeSection` (`echo`) to the engine's router.
//! The engine runs here, on a free loopback port, with a scratch config,
//! the device layer, `PipeWire` and the LAN off (`PATCHBAY_LAN=off`). One
//! test function: the engine and the family registrations are one per
//! process.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use futures::executor::block_on;
use patchbay::family::{ConnectCtx, ConnectError, ConnectFuture, DeviceFamily};
use patchbay::proto::{DeviceConfig, FamilyInfo, PatchbayError};
use patchbay::server::{Engine, EngineMode, EngineOptions};

pub mod probe_section {
    use patchbay::proto::PatchbayError;

    /// What the test family serves: one method, enough to see a call
    /// reach it.
    #[architect::rpc]
    pub trait ProbeSection {
        /// `text`, back, with the family's mark.
        async fn echo(&self, text: String) -> Result<String, PatchbayError>;
    }
}

pub mod unserved_section {
    use patchbay::proto::PatchbayError;

    /// A section no family on the engine serves.
    #[architect::rpc]
    pub trait UnservedSection {
        async fn ping(&self) -> Result<u32, PatchbayError>;
    }
}

use probe_section::{ProbeSection, ProbeSectionClient};
use unserved_section::UnservedSectionClient;

#[derive(Clone)]
struct Probe;

impl ProbeSection for Probe {
    async fn echo(&self, text: String) -> Result<String, PatchbayError> {
        Ok(format!("probe: {text}"))
    }
}

/// A family with a section of its own and no devices.
struct ProbeFamily;

impl DeviceFamily for ProbeFamily {
    fn kind(&self) -> &'static str {
        "probe"
    }

    fn info(&self) -> FamilyInfo {
        FamilyInfo::linked("probe", "Probe", &[], false)
    }

    fn routes(&self, router: architect::LayerRouter) -> architect::LayerRouter {
        router.with(
            probe_section::probe_section_rpc_service_descriptor(),
            probe_section::serve(Probe),
        )
    }

    fn connect(&self, _config: DeviceConfig, _ctx: ConnectCtx) -> ConnectFuture {
        Box::pin(async { Err(ConnectError::NotFound("a probe has no devices".to_owned())) })
    }
}

#[test]
fn a_familys_section_is_a_lane_on_the_one_connection_here_and_remote() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: before anything in this process reads the environment
    // (nextest runs each test in a process of its own).
    unsafe {
        std::env::set_var("PATCHBAY_CONFIG", tmp.path().join("patchbay.styx"));
        std::env::set_var("PATCHBAY_DEVICES", "off");
        std::env::set_var("PATCHBAY_PIPEWIRE", "off");
        std::env::set_var("PATCHBAY_LAN", "off");
    }
    let _ = patchbay::family::register(Arc::new(ProbeFamily));
    let mut opts = EngineOptions::new("127.0.0.1:0");
    opts.discover_peers = false;
    let engine = Engine::start_or_attach_blocking(opts).unwrap();
    let EngineMode::Owned {
        addr,
        serving: true,
    } = engine.mode()
    else {
        panic!("not an owned, serving engine: {:?}", engine.mode());
    };
    let url = format!("ws://{addr}/vox");

    block_on(async {
        // In this process: the engine's own lanes.
        let here = engine.lanes().open::<ProbeSectionClient>().await.unwrap();
        assert_eq!(here.echo("here".to_owned()).await.unwrap(), "probe: here");

        // Over the websocket: one connection, the core's service, its
        // stream and the family's section all lanes on it.
        let (core, _stream, lanes) =
            patchbay::server::spawn(async move { patchbay::server::dial(&url).await })
                .await
                .unwrap()
                .unwrap();
        let remote = lanes.open::<ProbeSectionClient>().await.unwrap();
        assert_eq!(
            remote.echo("there".to_owned()).await.unwrap(),
            "probe: there"
        );
        let core_answer = patchbay::server::spawn(async move { core.listen_address().await })
            .await
            .unwrap();
        assert!(core_answer.is_ok(), "the core's lane: {core_answer:?}");

        // A section no family here serves: refused, not hung.
        let opener = lanes.clone();
        let unserved =
            tokio_timeout(async move { opener.open::<UnservedSectionClient>().await }).await;
        match unserved {
            Ok(Err(_)) => {}
            Ok(Ok(client)) => {
                let call = tokio_timeout(async move { client.ping().await }).await;
                assert!(
                    !matches!(call, Ok(Ok(_))),
                    "an unserved section answered: {call:?}"
                );
            }
            Err(()) => panic!("opening an unserved section hung"),
        }
    });
}

/// `f` on the engine runtime, or `Err(())` after 5 s.
async fn tokio_timeout<F>(f: F) -> Result<F::Output, ()>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    patchbay::server::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), f)
            .await
            .map_err(|_| ())
    })
    .await
    .unwrap()
}
