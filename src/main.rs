extern crate sysmon;
extern crate toml;
extern crate getopts;
extern crate futures;
extern crate futures_cpupool;
extern crate tokio_timer;
#[macro_use]
extern crate log;
#[cfg(features = "watch")]
extern crate notify;

use sysmon::errors::*;
use sysmon::logger;
use sysmon::parsers::*;
use sysmon::plugin::*;
use sysmon::poller::Poller;
use sysmon::scheduler::*;
use sysmon::updater::Updater;

use futures::*;
use futures_cpupool::CpuPool;
use getopts::Options;
use std::env;
use std::fs;
use std::io::Read;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tokio_timer::Timer;

fn load_instance(
    plugins: &PluginRegistry, plugin_type: &String, section: &toml::Value
) -> Result<Box<Plugin>> {
    let plugin_key = parse_plugin_key(plugin_type.as_bytes()).to_full_result()?;

    let key = (plugin_key.plugin_kind.clone(), plugin_key.plugin_type.clone());

    let entry: &Entry = plugins
        .get(&key)
        .ok_or(ErrorKind::MissingPlugin(plugin_key.clone()))?;

    entry(&plugin_key, section.clone())
}

fn load_section(
    plugins: &PluginRegistry,
    section: toml::Value
) -> Result<Box<Plugin>> {
    let plugin_type: String = toml::decode(section.clone())
        .and_then(|value: toml::Table| {
            value.get("type").map(Clone::clone).and_then(toml::decode)
        })
        .ok_or(ErrorKind::TomlDecode)?;

    load_instance(plugins, &plugin_type, &section)
}

fn load_config(
    path: &String, plugins: &PluginRegistry
) -> Result<Vec<Box<Plugin>>>
{
    let mut file = fs::File::open(path)?;

    let mut content = String::new();
    file.read_to_string(&mut content)?;

    let mut parser = toml::Parser::new(&mut content);

    let config = match parser.parse() {
        Some(value) => value,
        None => {
            return Err(ErrorKind::TomlParse(parser.errors).into())
        }
    };

    let mut instances = Vec::new();

    for (section_key, section) in config.into_iter() {
        instances.push(load_section(plugins, section).chain_err(|| {
            ErrorKind::ConfigSection(section_key)
        })?);
    }

    Ok(instances)
}

fn print_usage(program: &str, plugins: &PluginRegistry, opts: Options) {
    let brief = format!("Usage: {} [options]", program);
    println!("{}", opts.usage(&brief));

    println!("Plugins:");

    for (&(ref kind, ref name), _) in plugins {
        println!("  {:?}:{}", kind, name);
    }
}

fn load_configs(
    configs: &Vec<String>,
    plugins: &PluginRegistry
) -> Result<Vec<Box<Plugin>>>
{
    let mut loaded: Vec<Box<Plugin>> = Vec::new();

    for config in configs {
        info!("loading: {}", config);

        loaded.extend(load_config(config, plugins).chain_err(|| {
            ErrorKind::Config(config.clone())
        })?);
    }

    Ok(loaded)
}

fn run() -> Result<()> {
    let mut opts = Options::new();

    opts.optflag("h", "help", "print this help");
    opts.optflag("", "debug", "enable debug logging");
    opts.optmulti("", "config", "load configuration file", "<file>");

    #[cfg(feature = "watch")]
    opts.optflag("w", "watch", "enable watching of the configuration directory");

    let args: Vec<String> = env::args().collect();
    let program = args[0].clone();

    let plugins: PluginRegistry = sysmon::plugins::load_plugins();

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => { m }
        Err(f) => {
            print_usage(&program, &plugins, opts);
            return Err(ErrorKind::Message(f.to_string()).into())
        }
    };

    if matches.opt_present("h") {
        print_usage(&program, &plugins, opts);
        return Ok(())
    }

    let level: log::LogLevelFilter = match matches.opt_present("debug") {
        true => log::LogLevelFilter::Debug,
        false => log::LogLevelFilter::Info,
    };

    logger::init(level)?;

    let configs = matches.opt_strs("config");

    let loaded = load_configs(&configs, &plugins)?;

    let pool = CpuPool::new(4);

    let timer = Arc::new(Timer::default());

    let framework = PluginFramework {
        cpupool: Rc::new(pool)
    };

    let mut instances = Vec::new();

    for plugin in loaded {
        instances.push(plugin.setup(&framework)?);
    }

    let poll_duration = Duration::new(5, 0);
    let update_duration = Duration::new(1, 0);

    let borrowed = Arc::new(instances);
    let polling = schedule(timer.clone(), poll_duration, Poller::new(borrowed.clone()));
    let updating = schedule(timer.clone(), update_duration, Updater::new(borrowed.clone()));

    info!("Started!");

    info!("Shutting down!");

    let _ = future::join_all(vec![polling, updating]).wait();

    Ok(())
}

fn main() {
    match run() {
        Err(e) => {
            println!("{}", e);

            for e in e.iter().skip(1) {
                println!("  caused by: {}", e);
            }

            if let Some(backtrace) = e.backtrace() {
                println!("  backtrace: {:?}", backtrace);
            }

            ::std::process::exit(1);
        },
        Ok(loaded) => loaded,
    }

    ::std::process::exit(0);
}
