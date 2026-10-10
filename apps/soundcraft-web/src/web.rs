//! The browser shell: the eframe web runner around [`SoundApp`].

use soundcraft_engine::Engine;
use soundcraft_ui_egui::{Services, SoundApp};
use std::sync::Arc;
use wasm_bindgen::JsCast as _;

const CANVAS_ID: &str = "soundcraft_canvas";
const LOADING_ID: &str = "soundcraft_loading";

pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("no document");
            return;
        };
        let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("missing <canvas id=\"{CANVAS_ID}\">");
            return;
        };
        let mut options = eframe::WebOptions::default();
        if query().contains("webgl")
            && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
        {
            create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
        }
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(move |cc| {
                    if let Some(rs) = &cc.wgpu_render_state {
                        log::info!("soundcraft-web: wgpu backend {:?}", rs.adapter.get_info().backend);
                    }
                    let engine = if query().contains("empty") { Engine::default() } else { soundcraft_engine::demo::demo_engine() };
                    let player = soundcraft_playback::Player::new(Arc::new(engine.session().clone()));
                    #[cfg_attr(not(feature = "icp-cloud"), allow(unused_mut))]
                    let mut app = SoundApp::new(engine, Some(player), Services::default());
                    #[cfg(feature = "icp-cloud")]
                    let cloud = {
                        let cloud = sovereign_cloud_ui::Cloud::new(sovereign_cloud_ui::AppSpec::SOUND, cc.egui_ctx.clone());
                        let menu = cloud.menu_handle();
                        app.services.file_menu = Some(Box::new(move |ui, has_doc| menu.ui(ui, has_doc)));
                        cloud
                    };
                    Ok(Box::new(WebShell(
                        app,
                        #[cfg(feature = "icp-cloud")]
                        cloud,
                    )))
                }),
            )
            .await;
        if let Some(el) = document.get_element_by_id(LOADING_ID) {
            match result {
                Ok(()) => el.remove(),
                Err(e) => el.set_inner_html(&format!("<p>SoundCraft failed to start: {e:?}</p><p>A browser with WebGPU or WebGL2 is required.</p>")),
            }
        }
    });
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

struct WebShell(SoundApp, #[cfg(feature = "icp-cloud")] sovereign_cloud_ui::Cloud);

impl eframe::App for WebShell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.logic(ctx);
        publish_dirty_state(self.0.engine.is_dirty() || self.0.engine.transport.recording);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.0.raw_input_hook(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
        #[cfg(feature = "icp-cloud")]
        self.1.ui(ui.ctx(), &mut crate::cloud::Adapter(&mut self.0));
        publish_dirty_state(self.0.engine.is_dirty() || self.0.engine.transport.recording);
    }
}

/// Browser-host contract: report unsaved work without exposing document contents.
fn publish_dirty_state(dirty: bool) {
    if let Some(root) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.document_element()) {
        let value = if dirty { "true" } else { "false" };
        if root.get_attribute("data-craft-dirty").as_deref() != Some(value) {
            let _ = root.set_attribute("data-craft-dirty", value);
        }
    }
}
