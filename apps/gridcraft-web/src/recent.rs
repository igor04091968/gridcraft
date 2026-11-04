use gridcraft_ui_egui::Inbox;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

const DATABASE: &str = "gridcraft";
const STORE: &str = "recent-files";

fn with_db(action: impl FnOnce(web_sys::IdbDatabase) + 'static) {
    let result = (|| {
        let factory = web_sys::window()?.indexed_db().ok()??;
        let request = factory.open_with_u32(DATABASE, 1).ok()?;
        let upgrade = Closure::once_into_js(move |event: web_sys::Event| {
            let Some(request) = event.target().and_then(|target| target.dyn_into::<web_sys::IdbOpenDbRequest>().ok()) else { return };
            let Ok(result) = request.result() else { return };
            let Ok(db) = result.dyn_into::<web_sys::IdbDatabase>() else { return };
            if !db.object_store_names().contains(STORE) {
                if let Err(e) = db.create_object_store(STORE) {
                    log::warn!("recent workbooks: cannot create store: {e:?}");
                }
            }
        });
        request.set_onupgradeneeded(Some(upgrade.unchecked_ref()));
        let success = Closure::once_into_js(move |event: web_sys::Event| {
            let Some(request) = event.target().and_then(|target| target.dyn_into::<web_sys::IdbOpenDbRequest>().ok()) else { return };
            match request.result().and_then(|value| value.dyn_into::<web_sys::IdbDatabase>()) {
                Ok(db) => action(db),
                Err(e) => log::warn!("recent workbooks: cannot open database: {e:?}"),
            }
        });
        request.set_onsuccess(Some(success.unchecked_ref()));
        Some(())
    })();
    if result.is_none() {
        log::warn!("recent workbooks: IndexedDB is unavailable");
    }
}

pub fn store(name: &str, bytes: &[u8]) {
    let name = name.to_string();
    let bytes = js_sys::Uint8Array::from(bytes);
    with_db(move |db| {
        let result = db
            .transaction_with_str_and_mode(STORE, web_sys::IdbTransactionMode::Readwrite)
            .and_then(|transaction| transaction.object_store(STORE))
            .and_then(|store| store.put_with_key(&bytes, &JsValue::from_str(&name)));
        if let Err(e) = result {
            log::warn!("recent workbooks: cannot store {name}: {e:?}");
        }
    });
}

pub fn open(name: &str, inbox: Inbox, ctx: egui::Context) {
    let name = name.to_string();
    with_db(move |db| {
        let request = db
            .transaction_with_str(STORE)
            .and_then(|transaction| transaction.object_store(STORE))
            .and_then(|store| store.get(&JsValue::from_str(&name)));
        let Ok(request) = request else {
            log::warn!("recent workbooks: cannot read {name}");
            return;
        };
        let success = Closure::once_into_js(move |event: web_sys::Event| {
            let Some(request) = event.target().and_then(|target| target.dyn_into::<web_sys::IdbRequest>().ok()) else { return };
            let Ok(value) = request.result() else { return };
            if value.is_undefined() {
                log::warn!("recent workbooks: no stored copy of {name}");
                return;
            }
            let bytes = js_sys::Uint8Array::new(&value).to_vec();
            inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((name.clone(), bytes));
            ctx.request_repaint();
        });
        request.set_onsuccess(Some(success.unchecked_ref()));
    });
}
