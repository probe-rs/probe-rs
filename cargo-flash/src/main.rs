#[macro_use] extern crate structopt;

use std::path::PathBuf;

use crate::structopt::StructOpt;

#[derive(Debug, StructOpt)]
struct Opt {
    #[structopt(name = "binary", long="bin")]
    bin: Option<String>,
    #[structopt(name = "example", long="example")]
    example: Option<String>,
    #[structopt(name = "package", short="p", long="package")]
    package: Option<String>,
    #[structopt(name = "release", long="release")]
    release: bool,
    #[structopt(name = "target", long="target")]
    target: Option<String>,
    #[structopt(name = "PATH", long="manifest-path", parse(from_os_str))]
    manifest_path: Option<PathBuf>,
}

fn main() {

    let opt = Opt::from_args();

    dbg!(&opt);

    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(path) = opt.manifest_path {
        cmd.manifest_path(path);
    }
    let metadata = cmd.exec().unwrap();

    let packages: Vec<cargo_metadata::Package> = if let Some(package) = opt.package {
        metadata.workspace_members
            .iter()
            .filter(|member| {
                if metadata[member].name == package {
                    true
                } else {
                    false
                }
            })
            .map(|member| metadata[member].clone())
            .collect()
    } else {
        metadata.workspace_members.iter().map(|member| metadata[member].clone()).collect()
    };

    if packages.len() > 1 {
        println!("Please specify the package.");
        std::process::exit(0);
    } else if packages.len() != 1 {
        println!("No matching packages found!");
        std::process::exit(0);
    } else {
        if let Some(example) = opt.example {
            let example_target = packages
                .iter()
                .find(|package| {
                    if package.targets.len() > 0 {
                        for target in &package.targets {
                            if target.kind.contains(&"example".to_string()) && target.name == example {
                                return true;
                            }
                        }
                        false
                    } else {
                        false
                    }
                });
            if example_target.is_some() {
                // TODO:
                println!("EXAMPLE");
            } else {
                println!("Example {} does not exist.", example);
                std::process::exit(0);
            }
        }
        
        if let Some(bin) = opt.bin {
            let bin_target = packages
                .iter()
                .find(|package| {
                    if package.targets.len() > 0 {
                        for target in &package.targets {
                            if target.kind.contains(&"bin".to_string()) && target.name == bin {
                                return true;
                            }
                        }
                        false
                    } else {
                        false
                    }
                });
            if bin_target.is_some() {
                // TODO:
                println!("BIN");
            } else {
                println!("Binary {} does not exist.", bin);
                std::process::exit(0);
            }
        } else {
            let default_bin_target = packages
                .iter()
                .find(|package| {
                    if package.targets.len() > 0 {
                        for target in &package.targets {
                            if target.kind.contains(&"bin".to_string()) {
                                return true;
                            }
                        }
                        false
                    } else {
                        false
                    }
                });
            if let Some(bin) = default_bin_target {
                dbg!(bin);
                dbg!(metadata.target_directory);
            } else {
                println!("Please specify a binary.");
                std::process::exit(0);
            }
        }
    }
}
