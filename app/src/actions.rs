//! Extra tray menu actions: update check / install and problem report.
//! They run their slow parts on background threads; dialogs are plain
//! message boxes, which are fine from any thread.

use crate::config::Config;
use crate::i18n::tr;
use crate::model::Hub;
use crate::notice::{Notice, NoticeKind};
use crate::tray::Action;
use crate::win::{self, wide};
use crate::{http, log, report, update};
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDNO, IDYES, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_YESNOCANCEL};

type Poster = Arc<dyn Fn(Notice) + Send + Sync>;

fn update_notice(r: &update::Release) -> Notice {
    let v = &r.version;
    Notice {
        kind: NoticeKind::Update,
        device_id: String::new(),
        device_name: String::new(),
        percent: None,
        hours_left: None,
        title: tr([
            format!("Batly {v} is available"),
            format!("Batly {v} est disponible"),
            format!("Batly {v} ist verfügbar"),
            format!("Batly {v} está disponible"),
            format!("Batly {v} è disponibile"),
            format!("Batly {v} está disponível"),
        ]
        .each_ref())
        .clone(),
        text: tr([
            "Right-click the icon → Check for updates to install it.",
            "Clic droit sur l'icône → Rechercher les mises à jour pour l'installer.",
            "Rechtsklick auf das Symbol → Nach Updates suchen, um es zu installieren.",
            "Clic derecho en el icono → Buscar actualizaciones para instalarla.",
            "Clic destro sull'icona → Cerca aggiornamenti per installarlo.",
            "Clique direito no ícone → Procurar atualizações para instalar.",
        ])
        .into(),
    }
}

/// Daily automatic check (no-op when disabled in the settings).
pub fn start_auto_check(cfg: &Config, known: Arc<Mutex<Option<update::Release>>>, poster: Poster) {
    if !cfg.updates.check {
        return;
    }
    let skipped = cfg.updates.skipped.clone();
    update::check_in_background(false, move |r| {
        if let Some(r) = r.filter(|r| r.version != skipped) {
            poster(update_notice(&r));
            *known.lock().unwrap() = Some(r);
        }
    });
}

fn ask(title: &str, text: &str, buttons: u32) -> i32 {
    // SAFETY: valid NUL-terminated strings, no owner window.
    unsafe { MessageBoxW(null_mut(), wide(text).as_ptr(), wide(title).as_ptr(), buttons | MB_SETFOREGROUND) }
}

pub fn update_action(cfg_path: PathBuf, known: Arc<Mutex<Option<update::Release>>>) -> Action {
    Action {
        label: tr([
            "Check for updates",
            "Rechercher les mises à jour",
            "Nach Updates suchen",
            "Buscar actualizaciones",
            "Cerca aggiornamenti",
            "Procurar atualizações",
        ])
        .into(),
        run: Box::new(move |_| {
            let known = known.clone();
            let cfg_path = cfg_path.clone();
            update::check_in_background(true, move |r| {
                let Some(r) = r.or_else(|| known.lock().unwrap().clone()) else {
                    let text = tr([
                        format!("Batly {} is up to date.", update::CURRENT),
                        format!("Batly {} est à jour.", update::CURRENT),
                        format!("Batly {} ist auf dem neuesten Stand.", update::CURRENT),
                        format!("Batly {} está actualizado.", update::CURRENT),
                        format!("Batly {} è aggiornato.", update::CURRENT),
                        format!("O Batly {} está atualizado.", update::CURRENT),
                    ]
                    .each_ref())
                    .clone();
                    ask("Batly", &text, MB_OK | MB_ICONINFORMATION);
                    return;
                };
                let v = &r.version;
                let notes: String = r.notes.chars().take(600).collect();
                let question = tr([
                    format!("Batly {v} is available (you have {}).\n\n{notes}\n\nYes: install now\nNo: skip this version\nCancel: later", update::CURRENT),
                    format!("Batly {v} est disponible (vous avez la {}).\n\n{notes}\n\nOui : installer maintenant\nNon : ignorer cette version\nAnnuler : plus tard", update::CURRENT),
                    format!("Batly {v} ist verfügbar (installiert: {}).\n\n{notes}\n\nJa: jetzt installieren\nNein: diese Version überspringen\nAbbrechen: später", update::CURRENT),
                    format!("Batly {v} está disponible (tienes la {}).\n\n{notes}\n\nSí: instalar ahora\nNo: omitir esta versión\nCancelar: más tarde", update::CURRENT),
                    format!("Batly {v} è disponibile (hai la {}).\n\n{notes}\n\nSì: installa ora\nNo: salta questa versione\nAnnulla: più tardi", update::CURRENT),
                    format!("O Batly {v} está disponível (você tem a {}).\n\n{notes}\n\nSim: instalar agora\nNão: ignorar esta versão\nCancelar: mais tarde", update::CURRENT),
                ]
                .each_ref())
                .clone();
                match ask("Batly", &question, MB_YESNOCANCEL | MB_ICONINFORMATION) {
                    IDYES => {
                        if let Err(e) = update::install(&r, |_| {}) {
                            win::message_box("Batly", &e);
                        }
                    }
                    IDNO => {
                        // The tray reloads the file by itself.
                        if let Ok(mut cfg) = Config::load(&cfg_path) {
                            cfg.updates.skipped = r.version.clone();
                            let _ = cfg.save(&cfg_path);
                        }
                        *known.lock().unwrap() = None;
                    }
                    _ => {}
                }
            });
        }),
    }
}

pub fn report_action(hub: Arc<Hub>, cfg_path: PathBuf) -> Action {
    Action {
        label: tr([
            "Report a problem…",
            "Signaler un problème…",
            "Problem melden…",
            "Informar de un problema…",
            "Segnala un problema…",
            "Relatar um problema…",
        ])
        .into(),
        run: Box::new(move |_| {
            let devices: Vec<String> = hub.snapshot().iter().map(http::json).collect();
            let devices_json = format!("[{}]", devices.join(","));
            let settings = std::fs::read_to_string(&cfg_path).unwrap_or_default();
            let log_path = log::path().unwrap_or_else(|| cfg_path.with_file_name("batly.log"));
            if let Err(e) = report::run(&devices_json, &settings, &log_path) {
                win::message_box("Batly", &e.to_string());
            }
        }),
    }
}
