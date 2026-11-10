mod app;
mod artwork;
mod daily;
mod desktop;
mod experience;
mod icons;
mod media;
mod motion;
mod oauth;
mod track_table;
mod updater;
mod vault;
mod widgets;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, ViewportBuilder};

pub fn run() -> eframe::Result {
    if updater::handle_command()
        .map_err(|error| eframe::Error::AppCreation(Box::new(std::io::Error::other(error))))?
    {
        return Ok(());
    }
    let Some(vault) = vault::Vault::open_or_activate()
        .map_err(|err| eframe::Error::AppCreation(Box::new(err)))?
    else {
        return Ok(());
    };
    let _apartment =
        media::Apartment::new().map_err(|err| eframe::Error::AppCreation(Box::new(err)))?;
    let mut viewport = ViewportBuilder::default()
        .with_inner_size([1200.0, 800.0])
        .with_min_inner_size([760.0, 560.0])
        .with_decorations(false);
    if let Some(icon) = icons::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "飞牛音乐 · FnMusic",
        options,
        Box::new(move |context| {
            install_chinese_fonts(&context.egui_ctx);
            let mut app = app::MusicApp::with_vault(context.egui_ctx.clone(), vault)?;
            use raw_window_handle::HasWindowHandle;
            if let Ok(handle) = context.window_handle()
                && let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw()
            {
                app.install_desktop(windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _));
            }
            Ok(Box::new(app))
        }),
    )
}

fn install_chinese_fonts(ctx: &egui::Context) {
    let Some(windows) = std::env::var_os("WINDIR").map(std::path::PathBuf::from) else {
        return;
    };
    let mut fonts = FontDefinitions::default();
    let mut primary = None;
    for name in ["msyh.ttc", "msyh.ttf", "simsun.ttc", "seguisym.ttf"] {
        if let Ok(bytes) = std::fs::read(windows.join("Fonts").join(name)) {
            if primary.is_none() && name.starts_with("msyh") {
                primary = Some(name.to_owned());
            }
            fonts
                .font_data
                .insert(name.into(), FontData::from_owned(bytes).into());
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(name.into());
            }
        }
    }
    if let Some(primary) = primary {
        let family = fonts.families.entry(FontFamily::Proportional).or_default();
        family.retain(|name| name != &primary);
        family.insert(0, primary);
    }
    ctx.set_fonts(fonts);
}
