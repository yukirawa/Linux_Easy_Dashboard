//! Application object: identity, stylesheet loading and the single window.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use log::{debug, error, info, warn};

use crate::config::{AppConfig, ConfigStore};
use crate::window::DashboardWindow;

pub const APP_ID: &str = "io.github.yukirawa.EasyDashboardMaker";
pub const APP_NAME: &str = "Easy Dashboard Maker";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn build() -> adw::Application {
    // Load libadwaita's stylesheet and named colours before any widget exists.
    if let Err(e) = adw::init() {
        warn!("libadwaita を初期化できません: {e}");
    }

    // `EDM_DEV_APP_ID` allows a second instance for development: GApplication
    // would otherwise refuse to start while the user's dashboard is running.
    let app_id = std::env::var("EDM_DEV_APP_ID").unwrap_or_else(|_| APP_ID.to_owned());
    let app = adw::Application::builder().application_id(app_id).build();

    app.connect_startup(|_| {
        load_stylesheet();
    });
    install_actions(&app);

    // The application quits when its last window closes, so a plain `Option`
    // holder is enough to keep the window alive across activations.
    let holder: Rc<RefCell<Option<Rc<DashboardWindow>>>> = Rc::new(RefCell::new(None));
    app.connect_activate(move |app| activate(app, &holder));

    app
}

fn activate(app: &adw::Application, holder: &Rc<RefCell<Option<Rc<DashboardWindow>>>>) {
    if let Some(window) = holder.borrow().as_ref() {
        window.present();
        return;
    }

    let store = match ConfigStore::new() {
        Ok(store) => store,
        Err(e) => {
            error!("設定ディレクトリを用意できません: {e:#}");
            app.quit();
            return;
        }
    };

    let (config, warning) = match store.load() {
        Ok(Some(config)) => (config, None),
        Ok(None) => {
            info!("初回起動のため初期レイアウトを使います");
            (AppConfig::starter(), None)
        }
        Err(e) => {
            warn!("レイアウトを読み込めません: {e:#}");
            (
                AppConfig::starter(),
                Some("保存されたレイアウトを読み込めなかったため、初期レイアウトで開始しました"),
            )
        }
    };

    let window = DashboardWindow::new(app, store, config);
    *holder.borrow_mut() = Some(window.clone());
    window.present();
    debug!("起動完了: {:?}", crate::startup_elapsed());

    crate::devtools::install_from_env(&window);

    if let Some(warning) = warning {
        window.notify(warning);
    }
}

fn install_actions(app: &adw::Application) {
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let weak = app.downgrade();
        move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.quit();
            }
        }
    });
    app.add_action(&quit);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate({
        let weak = app.downgrade();
        move |_, _| {
            if let Some(app) = weak.upgrade() {
                show_about(&app);
            }
        }
    });
    app.add_action(&about);

    app.set_accels_for_action("app.quit", &["<primary>q"]);
    app.set_accels_for_action("win.toggle-edit", &["<primary>e"]);
    app.set_accels_for_action("win.remove-selected", &["Delete"]);
}

fn show_about(app: &adw::Application) {
    let about = adw::AboutDialog::builder()
        .application_name(APP_NAME)
        .version(VERSION)
        .comments("ウィジェットを自由に配置できる、ネイティブ GTK4 のダッシュボード。")
        .copyright("© 2026 Yukirawa")
        .build();

    match app.active_window() {
        Some(window) => about.present(Some(&window)),
        None => about.present(None::<&gtk::Widget>),
    }
}

/// Loads the built in stylesheet, then an optional user override from
/// `$XDG_CONFIG_HOME/easy-dashboard-maker/style.css`.
fn load_stylesheet() {
    let Some(display) = gtk::gdk::Display::default() else {
        warn!("ディスプレイを開けないためスタイルを読み込みません");
        return;
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../data/style.css"));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let user_stylesheet = glib::user_config_dir()
        .join(crate::config::APP_DIR)
        .join("style.css");
    if user_stylesheet.exists() {
        let user_provider = gtk::CssProvider::new();
        user_provider.load_from_path(&user_stylesheet);
        gtk::style_context_add_provider_for_display(
            &display,
            &user_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        info!(
            "ユーザースタイルを適用しました: {}",
            user_stylesheet.display()
        );
    }
}
