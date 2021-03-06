use clap::{
    crate_description, crate_name, crate_version, value_t, value_t_or_exit, values_t, App, Arg,
};
use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::exit,
    process::Command,
};

struct Config {
    bpf_sdk: PathBuf,
    dump: bool,
    features: Vec<String>,
    manifest_path: Option<PathBuf>,
    no_default_features: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bpf_sdk: env::current_exe()
                .expect("Unable to get current directory")
                .parent()
                .expect("Unable to get parent directory")
                .to_path_buf()
                .join("sdk/bpf"),
            features: vec![],
            manifest_path: None,
            no_default_features: false,
            dump: true,
        }
    }
}

fn spawn<I, S>(program: &Path, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    print!("Running: {}", program.display());
    for arg in args.iter() {
        print!(" {}", arg.as_ref().to_str().unwrap_or("?"));
    }
    println!();

    let mut child = Command::new(program)
        .args(&args)
        .spawn()
        .unwrap_or_else(|err| {
            eprintln!("Failed to execute {}: {}", program.display(), err);
            exit(1);
        });

    let exit_status = child.wait().expect("failed to wait on child");
    if !exit_status.success() {
        exit(1);
    }
}

fn build_bpf(config: Config) {
    let mut metadata_command = cargo_metadata::MetadataCommand::new();
    if let Some(manifest_path) = config.manifest_path {
        metadata_command.manifest_path(manifest_path);
    }

    let metadata = metadata_command.exec().unwrap_or_else(|err| {
        eprintln!("Failed to obtain package metadata: {}", err);
        exit(1);
    });

    let root_package = metadata.root_package().unwrap_or_else(|| {
        eprintln!(
            "Workspace does not have a root package: {}",
            metadata.workspace_root.display()
        );
        exit(1);
    });

    let program_name = {
        let cdylib_targets = root_package
            .targets
            .iter()
            .filter_map(|target| {
                if target.crate_types.contains(&"cdylib".to_string()) {
                    Some(&target.name)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        match cdylib_targets.len() {
            0 => {
                println!(
                    "Note: {} crate does not contain a cdylib target",
                    root_package.name
                );
                None
            }
            1 => Some(cdylib_targets[0].replace("-", "_")),
            _ => {
                eprintln!(
                    "{} crate contains multiple cdylib targets: {:?}",
                    root_package.name, cdylib_targets
                );
                exit(1);
            }
        }
    };

    let legacy_program_feature_present = root_package.features.contains_key("program");
    let root_package_dir = &root_package.manifest_path.parent().unwrap_or_else(|| {
        eprintln!(
            "Unable to get directory of {}",
            root_package.manifest_path.display()
        );
        exit(1);
    });

    let target_build_directory = metadata
        .target_directory
        .join("bpfel-unknown-unknown/release");

    env::set_current_dir(&root_package_dir).unwrap_or_else(|err| {
        eprintln!(
            "Unable to set current directory to {}: {}",
            root_package_dir.display(),
            err
        );
        exit(1);
    });

    println!("BPF SDK: {}", config.bpf_sdk.display());
    if config.no_default_features {
        println!("No default features");
    }
    if !config.features.is_empty() {
        println!("Features: {}", config.features.join(" "));
    }
    if legacy_program_feature_present {
        println!("Legacy program feature detected");
    }

    let xargo_build = config.bpf_sdk.join("rust/xargo-build.sh");
    let mut spawn_args = vec![];

    if config.no_default_features {
        spawn_args.push("--no-default-features");
    }
    for feature in &config.features {
        spawn_args.push("--features");
        spawn_args.push(feature);
    }
    if legacy_program_feature_present {
        if !config.no_default_features {
            spawn_args.push("--no-default-features");
        }
        spawn_args.push("--features=program");
    }
    spawn(&config.bpf_sdk.join(xargo_build), &spawn_args);

    if let Some(program_name) = program_name {
        let program_unstripped_so = target_build_directory.join(&format!("{}.so", program_name));
        let program_dump = PathBuf::from(format!("{}-dump.txt", program_name));
        let program_so = PathBuf::from(format!("{}.so", program_name));

        spawn(
            &config.bpf_sdk.join("scripts/strip.sh"),
            &[&program_unstripped_so, &program_so],
        );

        if config.dump {
            spawn(
                &config.bpf_sdk.join("scripts/dump.sh"),
                &[&program_unstripped_so, &program_dump],
            );
        }
    } else if config.dump {
        println!("Note: --dump is only available for crates with a cdylib target");
    }
}

fn main() {
    let default_bpf_sdk = format!("{}", Config::default().bpf_sdk.display());

    let mut args = env::args().collect::<Vec<_>>();
    // When run as a cargo subcommand, the first program argument is the subcommand name.
    // Remove it
    if let Some(arg1) = args.get(1) {
        if arg1 == "build-bpf" {
            args.remove(1);
        }
    }

    let matches = App::new(crate_name!())
        .about(crate_description!())
        .version(crate_version!())
        .arg(
            Arg::with_name("bpf_sdk")
                .long("bpf-sdk")
                .value_name("PATH")
                .takes_value(true)
                .default_value(&default_bpf_sdk)
                .help("Path to the Solana BPF SDK"),
        )
        .arg(
            Arg::with_name("dump")
                .long("dump")
                .takes_value(false)
                .help("Dump ELF information to a text file on success"),
        )
        .arg(
            Arg::with_name("features")
                .long("features")
                .value_name("FEATURES")
                .takes_value(true)
                .multiple(true)
                .help("Space-separated list of features to activate"),
        )
        .arg(
            Arg::with_name("no_default_features")
                .long("no-default-features")
                .takes_value(false)
                .help("Do not activate the `default` feature"),
        )
        .arg(
            Arg::with_name("manifest_path")
                .long("manifest-path")
                .value_name("PATH")
                .takes_value(true)
                .help("Path to Cargo.toml"),
        )
        .get_matches_from(args);

    let bpf_sdk = value_t_or_exit!(matches, "bpf_sdk", PathBuf);

    let config = Config {
        bpf_sdk: fs::canonicalize(&bpf_sdk).unwrap_or_else(|err| {
            eprintln!(
                "BPF SDK path does not exist: {}: {}",
                bpf_sdk.display(),
                err
            );
            exit(1);
        }),
        dump: matches.is_present("dump"),
        features: values_t!(matches, "features", String)
            .ok()
            .unwrap_or_else(Vec::new),
        manifest_path: value_t!(matches, "manifest_path", PathBuf).ok(),
        no_default_features: matches.is_present("no_default_features"),
    };
    build_bpf(config);
}
