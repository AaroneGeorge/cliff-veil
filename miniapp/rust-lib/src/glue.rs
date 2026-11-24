//! Logos glue. The builder derives the module's `.lidl` from the `CliffVeilModule` trait and generates the C ABI
//! around it. Every method answers one JSON text: `{"ok": true, ...}` or `{"ok": false, "error": {...}}`.

// The generated scaffold exports C-ABI entry points; this file adds none of its own.
#![allow(unsafe_code)]

use cliff_veil_miniapp::app::App;
use cliff_veil_miniapp::backend::CliBackend;

pub trait CliffVeilModule: Send + 'static {
    /// `{"cli": "<path of cliff-veil>", "env": {"LEE_WALLET_HOME_DIR": "...", "CV_IDS": "...", ...}}`.
    fn configure(&mut self, settings_json: String) -> String;
    /// Recipient view: the schedules paying `beneficiary`, with totals per asset. `schedules`: schedule accounts to
    /// show, one per line (empty: list them with the command line).
    fn recipient_view(&mut self, beneficiary: String, schedules: String) -> String;
    /// Recipient view of one schedule.
    fn position(&mut self, schedule: String) -> String;
    /// Creator view: the active schedules of `creator` (as creator or authority), with the actions it may take.
    /// `schedules` as for `recipient_view`.
    fn creator_view(&mut self, creator: String, schedules: String) -> String;
    /// The SDK's privacy disclosure, read from the command line (also checks the Settings).
    fn disclosure(&mut self) -> String;
    /// Pre-claim summary (claimable, estimated fee, gas check) and, on the private path, the disclosure.
    fn prepare_claim(&mut self, request_json: String) -> String;
    /// Acknowledges the disclosure shown for claim `flow`; `digest` is the one `prepare_claim` returned.
    fn acknowledge_disclosure(&mut self, flow: i64, digest: String) -> String;
    /// Sends claim `flow` if nothing blocks it (a private claim needs its disclosure acknowledged).
    fn confirm_claim(&mut self, flow: i64) -> String;
    /// Checks the create form: review lines or field errors.
    fn validate_create(&mut self, form_json: String) -> String;
    /// Create, step 1: checks and prices it.
    fn prepare_create(&mut self, form_json: String) -> String;
    /// Cancel, step 1.
    fn prepare_cancel(&mut self, schedule: String, authority: String) -> String;
    /// Signal milestone `index`, step 1.
    fn prepare_signal(&mut self, schedule: String, authority: String, index: i64) -> String;
    /// Create, cancel or signal, step 2: sends it.
    fn confirm_op(&mut self, op: i64) -> String;

    fn on_context_ready(&mut self, _ctx: &RustModuleContext) {}
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/generated/provider_gen.rs"));

struct CliffVeil {
    app: App<CliBackend>,
}

impl Default for CliffVeil {
    fn default() -> Self {
        // `CLIFF_VEIL_CLI` sets the command line before the view's Settings tab does.
        let program = std::env::var("CLIFF_VEIL_CLI").unwrap_or_default();
        Self { app: App::new(CliBackend { program, env: Vec::new() }) }
    }
}

fn id(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

impl CliffVeilModule for CliffVeil {
    fn configure(&mut self, settings_json: String) -> String {
        self.app.configure(&settings_json)
    }
    fn recipient_view(&mut self, beneficiary: String, schedules: String) -> String {
        self.app.recipient_view(&beneficiary, &schedules)
    }
    fn position(&mut self, schedule: String) -> String {
        self.app.position(&schedule)
    }
    fn creator_view(&mut self, creator: String, schedules: String) -> String {
        self.app.creator_view(&creator, &schedules)
    }
    fn disclosure(&mut self) -> String {
        self.app.disclosure()
    }
    fn prepare_claim(&mut self, request_json: String) -> String {
        self.app.prepare_claim(&request_json)
    }
    fn acknowledge_disclosure(&mut self, flow: i64, digest: String) -> String {
        self.app.acknowledge_disclosure(id(flow), &digest)
    }
    fn confirm_claim(&mut self, flow: i64) -> String {
        self.app.confirm_claim(id(flow))
    }
    fn validate_create(&mut self, form_json: String) -> String {
        self.app.validate_create(&form_json)
    }
    fn prepare_create(&mut self, form_json: String) -> String {
        self.app.prepare_create(&form_json)
    }
    fn prepare_cancel(&mut self, schedule: String, authority: String) -> String {
        self.app.prepare_cancel(&schedule, &authority)
    }
    fn prepare_signal(&mut self, schedule: String, authority: String, index: i64) -> String {
        match u8::try_from(index) {
            Ok(i) => self.app.prepare_signal(&schedule, &authority, i),
            Err(_) => r#"{"ok":false,"error":{"code":"bad_request","message":"milestone index out of range"}}"#.to_owned(),
        }
    }
    fn confirm_op(&mut self, op: i64) -> String {
        self.app.confirm_op(id(op))
    }
}

#[no_mangle]
pub extern "Rust" fn logos_module_install() {
    install::<CliffVeil>();
}
