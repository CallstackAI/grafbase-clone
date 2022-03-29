use quick_error::quick_error;
// FIXME: To keep Clippy happy.
pub use log_;

use std::sync::atomic::AtomicBool;

#[cfg(feature = "with-worker")]
pub use worker;

quick_error! {
    #[derive(Debug)]
    pub enum Error {
        DatadogRequest(err: surf::Error) {
            display("{}", err)
        }
        DatadogPushFailed(response: String) {
            display("{}", response)
        }
    }
}

#[derive(strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum LogSeverity {
    Debug,
    Info,
    Error,
}

pub static ENABLE_LOGGING: AtomicBool = AtomicBool::new(false);

thread_local! {
    pub static LOG_ENTRIES: std::cell::RefCell<Vec<(String, LogSeverity, String)>> =
        std::cell::RefCell::new(Vec::new());
}

#[macro_export]
macro_rules! log {
    ($status:expr, $request_id:expr, $($t:tt)*) => { {
        let message = format_args!($($t)*).to_string();
        #[cfg(feature = "with-worker")]
        match status {
            LogSeverity::Debug =>
                $crate::worker::console_debug!("{}", message),
            LogSeverity::Info =>
                $crate::worker::console_log!("{}", message),
            LogSeverity::Error =>
                $crate::worker::console_error!("{}", message),
        }
        if $crate::ENABLE_LOGGING.load(std::sync::atomic::Ordering::Relaxed) {
            $crate::LOG_ENTRIES.with(|log_entries| log_entries
                .try_borrow_mut()
                .expect("reentrance is impossible in our single-threaded runtime")
                .push(($request_id.to_string(), $status, message)));
        }
    } }
}

#[macro_export]
macro_rules! debug {
    ($request_id:expr, $($t:tt)*) => { {
        $crate::log!($crate::LogSeverity::Debug, $request_id, $($t)*);
        $crate::log_::debug!($($t)*);
    } }
}

#[macro_export]
macro_rules! info {
    ($request_id:expr, $($t:tt)*) => { {
        $crate::log!($crate::LogSeverity::Info, $request_id, $($t)*);
        $crate::log_::info!($($t)*);
    } }
}

#[macro_export]
macro_rules! error {
    ($request_id:expr, $($t:tt)*) => { {
        $crate::log!($crate::LogSeverity::Error, $request_id, $($t)*);
        $crate::log_::error!($($t)*);
    } }
}

#[derive(serde::Serialize)]
pub struct DatadogLogEntry {
    ddsource: String,
    ddtags: String,
    hostname: String,
    message: String,
    service: String,
    status: String,
}

pub fn set_logging_enabled(enabled: bool) {
    ENABLE_LOGGING.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub struct LogConfig {
    pub api_key: String,
    pub service_name: &'static str,
    pub environment: String,
    pub branch: Option<String>,
}

fn collect_logs_to_be_pushed(
    log_config: &LogConfig,
    request_id: &str,
    request_host_name: &str,
) -> Vec<DatadogLogEntry> {
    #[rustfmt::skip]
    let mut tags = vec![
        ("request_id", request_id),
        ("environment", &log_config.environment),
    ];
    if let Some(branch) = log_config.branch.as_ref() {
        tags.push(("branch", branch.as_str()));
    }
    let tag_string = tags
        .iter()
        .map(|(lhs, rhs)| format!("{}:{}", lhs, rhs))
        .collect::<Vec<_>>()
        .join(",");

    let entries = LOG_ENTRIES.with(|log_entries| {
        log_entries
            .try_borrow_mut()
            .expect("reentrance is impossible in our single-threaded runtime")
            .iter()
            // FIXME: Replace with `Vec::drain_filter()` when it's stable.
            .filter(|(entry_request_id, _, _)| entry_request_id == request_id)
            .map(|(_, severity, message)| DatadogLogEntry {
                ddsource: "grafbase.api".to_owned(),
                ddtags: tag_string.clone(),
                hostname: request_host_name.to_owned(),
                message: message.clone(),
                service: log_config.service_name.to_owned(),
                status: severity.to_string(),
            })
            .collect::<Vec<_>>()
    });

    LOG_ENTRIES.with(|log_entries| {
        log_entries
            .try_borrow_mut()
            .expect("reentrance is impossible in our single-threaded runtime")
            .retain(|(entry_request_id, _, _)| entry_request_id != request_id)
    });

    entries
}

pub async fn push_logs_to_datadog(
    log_config: LogConfig,
    request_id: String,
    request_host_name: String,
) -> Result<(), Error> {
    if !ENABLE_LOGGING.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }

    let entries = collect_logs_to_be_pushed(&log_config, &request_id, &request_host_name);

    const URL: &str = "https://http-intake.logs.datadoghq.com/api/v2/logs";

    let mut res = surf::post(URL)
        .header("DD-API-KEY", &log_config.api_key)
        .body_json(&entries)
        .map_err(Error::DatadogRequest)?
        .send()
        .await
        .map_err(Error::DatadogRequest)?;

    if !res.status().is_success() {
        let response = res
            .body_string()
            .await
            .expect("must be able to get the response as a string");
        return Err(Error::DatadogPushFailed(response));
    }

    Ok(())
}
