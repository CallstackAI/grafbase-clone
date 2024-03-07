#![cfg_attr(test, allow(unused_crate_dependencies))]
#![forbid(unsafe_code)]

mod build;
mod check;
mod cli_input;
mod create;
mod deploy;
mod dev;
mod dump_config;
mod errors;
mod init;
mod introspect;
mod link;
mod login;
mod logout;
mod logs;
mod output;
mod panic_hook;
mod prompts;
mod publish;
mod schema;
mod start;
mod subgraphs;
mod trust;
mod unlink;
mod watercolor;

#[macro_use]
extern crate log;

use crate::{
    build::build,
    cli_input::{Args, ArgumentNames, FederatedSubCommand, LogsCommand, SubCommand},
    create::create,
    deploy::deploy,
    dev::dev,
    init::init,
    link::link,
    login::login,
    logout::logout,
    logs::logs,
    start::start,
    unlink::unlink,
};
use clap::Parser;
use common::{analytics::Analytics, environment::Environment};
use errors::CliError;
use output::report;
use std::{process, thread};
use toml as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{fmt, prelude::*, reload, EnvFilter};
use watercolor::ShouldColorize;

use mimalloc::MiMalloc;
use tokio::runtime::Handle;
use tracing::Subscriber;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() {
    panic_hook!();

    rustls::crypto::ring::default_provider().install_default().unwrap();

    let args = Args::parse();
    ShouldColorize::from_env();

    let exit_code = match try_main(args) {
        Ok(()) => 0,
        Err(error) => {
            report::error(&error);
            1
        }
    };

    process::exit(exit_code);
}

fn try_main(args: Args) -> Result<(), CliError> {
    let filter = EnvFilter::builder().parse_lossy(args.log_filter());
    let (otel_layer, reload_handle) = grafbase_tracing::otel::layer::new_noop();

    tracing_subscriber::registry()
        .with(matches!(args.command, SubCommand::Dev(..) | SubCommand::Start(..)).then_some(otel_layer))
        .with(fmt::layer())
        .with(filter)
        .init();

    trace!("subcommand: {}", args.command);

    // do not display header if we're in a pipe
    if atty::is(atty::Stream::Stdout) {
        report::cli_header();
    }

    if args.command.in_project_context() {
        Environment::try_init_with_project(args.home).map_err(CliError::CommonError)?;
    } else if !args.command.runs_production_server() {
        // TODO: temporary if clause
        Environment::try_init(args.home).map_err(CliError::CommonError)?;
    }

    // TODO: temporary
    if !args.command.runs_production_server() {
        Analytics::init().map_err(CliError::CommonError)?;
        Analytics::command_executed(args.command.as_ref(), args.command.argument_names());
        report::warnings(&Environment::get().warnings);
    }

    match args.command {
        SubCommand::Completions(cmd) => {
            cmd.shell.completions();

            Ok(())
        }
        SubCommand::Dev(cmd) => {
            // ignoring any errors to fall back to the normal handler if there's an issue
            let _set_handler_result = ctrlc::set_handler(|| {
                report::goodbye();
                process::exit(exitcode::OK);
            });

            // FIXME: dev for now doesn't use but should in the future
            let (reload_tx, reload_rx) = oneshot::channel::<Handle>();
            otel_reload(
                reload_handle,
                reload_rx,
                &TelemetryConfig {
                    service_name: "grafbase_dev".to_string(),
                    tracing: Default::default(),
                },
            );

            dev(
                cmd.search,
                !cmd.disable_watch,
                cmd.subgraph_port(),
                cmd.log_levels(),
                args.trace >= 2,
                reload_tx,
            )
        }
        SubCommand::Init(cmd) => init(cmd.name(), cmd.template(), cmd.graph),
        SubCommand::Login => login(),
        SubCommand::Logout => logout(),
        SubCommand::Create(cmd) => create(&cmd.create_arguments()),
        SubCommand::Deploy => deploy(),
        SubCommand::Link(cmd) => link(cmd.project),
        SubCommand::Unlink => unlink(),
        SubCommand::Logs(LogsCommand {
            project_branch,
            limit,
            no_follow,
        }) => logs(project_branch, limit, !no_follow),
        SubCommand::Federated(cmd) => match cmd.command {
            FederatedSubCommand::Start(cmd) => {
                let _ = ctrlc::set_handler(|| {
                    report::goodbye();
                    process::exit(exitcode::OK);
                });

                production_server::start(cmd.listen_address, &cmd.config, cmd.fetch_method()?)
                    .map_err(CliError::ProductionServerError)
            }
        },
        SubCommand::Start(cmd) => {
            let _ = ctrlc::set_handler(|| {
                report::goodbye();
                process::exit(exitcode::OK);
            });

            let toml_config = cmd.config()?;
            let (reload_tx, reload_rx) = oneshot::channel::<Handle>();

            if let Some(telemetry_config) = toml_config.telemetry.as_ref() {
                otel_reload(reload_handle, reload_rx, telemetry_config);
            }

            start(
                cmd.listen_address(),
                cmd.log_levels(),
                cmd.federated_schema_path(),
                args.trace >= 2,
                reload_tx,
            )
        }
        SubCommand::Build(cmd) => {
            let _ = ctrlc::set_handler(|| {
                report::goodbye();
                process::exit(exitcode::OK);
            });

            build(cmd.parallelism(), args.trace >= 2)
        }
        SubCommand::Subgraphs(cmd) => subgraphs::subgraphs(cmd),
        SubCommand::Schema(cmd) => schema::schema(cmd),
        SubCommand::Publish(cmd) => {
            if cmd.dev {
                report::publishing();
                match federated_dev::add_subgraph(
                    &cmd.subgraph_name,
                    &cmd.url,
                    cmd.dev_api_port,
                    cmd.headers().collect(),
                ) {
                    Ok(_) => {
                        report::publish_command_success(&cmd.subgraph_name);
                        Ok(())
                    }
                    Err(federated_dev::Error::Internal(error)) => {
                        report::local_publish_command_failure(&cmd.subgraph_name, &error.to_string());
                        Ok(())
                    }
                    Err(federated_dev::Error::SubgraphComposition(error)) => {
                        report::local_publish_command_failure(&cmd.subgraph_name, &error.to_string());
                        Ok(())
                    }
                    Err(other) => Err(CliError::Publish(other.to_string())),
                }
            } else {
                publish::publish(cmd)
            }
        }
        SubCommand::Introspect(cmd) => introspect::introspect(&cmd),
        SubCommand::DumpConfig => dump_config::dump_config(),
        SubCommand::Check(cmd) => check::check(cmd),
        SubCommand::Trust(cmd) => trust::trust(cmd),
    }
}

fn otel_reload<S>(
    reload_handle: reload::Handle<FilteredLayer<S>, S>,
    reload_rx: oneshot::Receiver<Handle>,
    telemetry_config: &TelemetryConfig,
) where
    S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync,
{
    let otel_service_name = telemetry_config.service_name.clone();
    let tracing_config = telemetry_config.tracing.clone();

    thread::spawn(move || match reload_rx.recv() {
        Ok(rt_handle) => {
            debug!("reloading otel layer");
            // new_batched will use the tokio runtime for its internals
            rt_handle.spawn(async move {
                // unfortunately I have to set the filters here due to: https://github.com/tokio-rs/tracing/issues/1629
                let sampling_filter = RatioSamplingFilter::new(tracing_config.sampling);
                let env_filter = EnvFilter::new(&tracing_config.filter);

                // create the batched layer
                let otel_layer =
                    grafbase_tracing::otel::layer::new_batched::<S, Tokio>(otel_service_name, tracing_config, Tokio)
                        .expect("should successfully build a batched otel layer for tracing");

                // replace the existing layer with the new one and update its filters
                // the explicit filters update shouldn't be required but the bug mentioned above makes it so
                reload_handle
                    .modify(|layer| {
                        *layer.inner_mut() = otel_layer;
                        *layer.filter_mut() = FilterExt::boxed(sampling_filter.and(env_filter));
                    })
                    .expect("should successfully reload otel layer");
            });
        }
        Err(e) => {
            warn!("received an error while waiting for otel reload: {e}");
        }
    });
}
