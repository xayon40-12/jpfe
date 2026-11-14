//! Interactive vocabulary statistics for several books, as a desktop app or a web page.

mod app;
mod chart;
mod book;
mod platform;
mod reader;
mod storage;
mod theme;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("jpfe")
            .with_inner_size([1280.0, 880.0])
            .with_min_inner_size([900.0, 620.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("jpfe viewer", options, Box::new(|cc| Ok(Box::new(app::ViewerApp::new(cc)))))
}

#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast;
    wasm_bindgen_futures::spawn_local(async {
        let canvas = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("app"))
            .expect("index.html has a canvas with id 'app'")
            .unchecked_into::<web_sys::HtmlCanvasElement>();
        let result = eframe::WebRunner::new()
            .start(canvas, eframe::WebOptions::default(), Box::new(|cc| Ok(Box::new(app::ViewerApp::new(cc)))))
            .await;
        if let Err(e) = result {
            log::error!("Failed to start: {e:?}");
        }
    });
}
