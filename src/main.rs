use anyhow::{Context, Result};
use cargo_metadata::{Metadata, MetadataCommand, PackageId};
use clap::Parser;
use fs_extra::dir::{self, CopyOptions};
use std::collections::HashMap;
use std::fs::{self, remove_file};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, Value};
use walkdir::WalkDir;

#[derive(Parser)]
#[clap(name = "cargo-localize", about = "Localizes all dependencies into a 3rd-party folder")]
struct Args {
    #[clap(default_value = ".")]
    project_path: PathBuf,
    #[clap(long, default_value = "3rd-party")]
    third_party_dir: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let project_path = args.project_path.canonicalize().context("Invalid project path")?;
    let third_party_path = project_path.join(&args.third_party_dir);

    println!("Running cargo fetch...");
    std::process::Command::new("cargo")
        .arg("fetch")
        .current_dir(&project_path)
        .status()
        .context("Failed to run cargo fetch")?;

    println!("Getting metadata...");
    let metadata = MetadataCommand::new()
        .manifest_path(project_path.join("Cargo.toml"))
        .exec()
        .context("Failed to get cargo metadata")?;

    fs::create_dir_all(&third_party_path).context("Failed to create 3rd-party directory")?;

    println!("Copying dependencies...");
    copy_dependencies(&metadata, &third_party_path)?;

    println!("Updating Cargo.toml files...");
    update_cargo_toml(&metadata, &project_path, &third_party_path)?;

    let lock_file = project_path.join("Cargo.lock");
    if lock_file.exists() {
        remove_file(&lock_file).context("Failed to remove Cargo.lock")?;
    }

    println!("Dependencies localized to {}", third_party_path.display());
    Ok(())
}

fn copy_dependencies(metadata: &Metadata, third_party_path: &Path) -> Result<()> {
    // Try multiple possible cargo registry locations
    let possible_cargo_homes = vec![
        dirs::home_dir().map(|p| p.join(".cargo/registry/src")),
        std::env::var("CARGO_HOME").ok().map(|p| PathBuf::from(p).join("registry/src")),
    ];

    let cargo_home = possible_cargo_homes
        .into_iter()
        .find_map(|p| p.filter(|path| path.exists()))
        .context("Failed to find Cargo registry directory")?;

    println!("Using cargo registry: {}", cargo_home.display());

    // Create a map of PackageId to Package for quick lookup
    let package_map: HashMap<PackageId, &cargo_metadata::Package> = metadata
        .packages
        .iter()
        .map(|p| (p.id.clone(), p))
        .collect();

    // Get the resolved dependency graph
    let resolve = metadata.resolve.as_ref().context("No resolve data in metadata")?;

    for node in &resolve.nodes {
        let package = package_map
            .get(&node.id)
            .context(format!("Package {} not found in metadata", node.id))?;

        // Skip workspace packages
        if is_workspace_package(package, metadata.workspace_root.as_std_path()) {
            println!("Skipping workspace package: {}", package.name);
            continue;
        }

        println!(
            "Processing dependency: {} v{} with features: {:?}",
            package.name, package.version, node.features
        );

        let source_path = find_crate_source(&cargo_home, &package.name, &package.version.to_string())?;
        let dest_name = format!("{}-{}", package.name, package.version);
        let dest_path = third_party_path.join(&dest_name);

        if dest_path.exists() {
            println!("  Already exists: {}", dest_path.display());
            continue;
        }

        let options = CopyOptions::new().overwrite(true);
        dir::copy(&source_path, &third_party_path, &options).context(format!(
            "Failed to copy {} to {}",
            source_path.display(),
            third_party_path.display()
        ))?;

        println!("  Copied: {} -> {}", source_path.display(), dest_path.display());
    }

    Ok(())
}

fn is_workspace_package(package: &cargo_metadata::Package, workspace_root: &Path) -> bool {
    // Check if the package manifest is within the workspace
    package.manifest_path.starts_with(workspace_root)
}

fn find_crate_source(cargo_home: &Path, name: &str, version: &str) -> Result<PathBuf> {
    println!("  Looking for crate source: {}-{}", name, version);
    
    // Look in all registry source directories
    for registry_entry in fs::read_dir(cargo_home)? {
        let registry_entry = registry_entry?;
        if !registry_entry.file_type()?.is_dir() {
            continue;
        }

        let registry_path = registry_entry.path();
        println!("    Searching in registry: {}", registry_path.display());

        // Search for the specific crate version
        for entry in WalkDir::new(&registry_path)
            .max_depth(2)
            .into_iter()
            .filter_entry(|e| e.file_type().is_dir())
        {
            let entry = entry?;
            let path = entry.path();
            
            if let Some(dir_name) = path.file_name() {
                let dir_name_str = dir_name.to_string_lossy();
                
                // Match exact version: crate-name-version
                if dir_name_str == format!("{}-{}", name, version) {
                    println!("    Found: {}", path.display());
                    return Ok(path.to_path_buf());
                }
            }
        }
    }
    
    Err(anyhow::anyhow!("Crate {}:{} not found in Cargo registry at {}", name, version, cargo_home.display()))
}

fn update_cargo_toml(metadata: &Metadata, project_path: &Path, third_party_path: &Path) -> Result<()> {
    // Always update the main Cargo.toml
    println!("Updating main Cargo.toml");
    update_single_cargo_toml(&metadata, &project_path.join("Cargo.toml"), project_path, third_party_path)?;

    // Update Cargo.toml files for each copied dependency
    for package in &metadata.packages {
        if is_workspace_package(package, metadata.workspace_root.as_std_path()) {
            continue;
        }
        
        let crate_dir_name = format!("{}-{}", package.name, package.version);
        let cargo_toml_path = third_party_path.join(&crate_dir_name).join("Cargo.toml");
        
        if cargo_toml_path.exists() {
            println!("Updating dependency Cargo.toml: {}", cargo_toml_path.display());
            update_single_cargo_toml(&metadata, &cargo_toml_path, project_path, third_party_path)?;
        }
    }

    Ok(())
}

fn update_single_cargo_toml(
    metadata: &Metadata,
    cargo_toml_path: &Path,
    _project_path: &Path,
    third_party_path: &Path,
) -> Result<()> {
    let bak_filepath = cargo_toml_path.to_string_lossy().to_string() + ".bak";
    if !fs::exists(&bak_filepath).is_ok_and(|v| v) {
        fs::copy(cargo_toml_path, bak_filepath).context("Failed to backup Cargo.toml to Cargo.toml.bak")?;
    }
    let content = fs::read_to_string(cargo_toml_path).context("Failed to read Cargo.toml")?;
    let mut doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Process all dependency sections
    let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
    for section in &sections {
        if let Some(deps) = doc.get_mut(section).and_then(|t| t.as_table_mut()) {
            update_dependencies(deps, metadata, cargo_toml_path, third_party_path)?;
        }
    }

    // Process target-specific dependencies
    if let Some(target_table) = doc.get_mut("target").and_then(|t| t.as_table_mut()) {
        for (_, target_value) in target_table.iter_mut() {
            if let Some(target_spec) = target_value.as_table_mut() {
                for section in &sections {
                    if let Some(deps) = target_spec.get_mut(section).and_then(|t| t.as_table_mut()) {
                        update_dependencies(deps, metadata, cargo_toml_path, third_party_path)?;
                    }
                }
            }
        }
    }

    fs::write(cargo_toml_path, doc.to_string()).context("Failed to write Cargo.toml")?;
    
    let orig_filepath = cargo_toml_path.to_string_lossy().to_string() + ".orig";
    if fs::exists(&orig_filepath).is_ok_and(|v| v) {
        fs::remove_file(orig_filepath).context("Failed to remove Cargo.toml.orig")?;
    }
    
    Ok(())
}

fn update_dependencies(
    deps: &mut Table,
    metadata: &Metadata,
    cargo_toml_path: &Path,
    third_party_path: &Path,
) -> Result<()> {
    for (dep_name, dep_value) in deps.iter_mut() {
        println!("  Processing dependency: {}", dep_name);

        match dep_value {
            Item::Value(Value::String(_)) => {
                // Simple version string dependency
                let package_info = find_package_for_dependency(metadata, dep_name.get(), None);
                if let Some((package, features)) = package_info {
                    let crate_dir_name = format!("{}-{}", package.name, package.version);
                    let dep_path = third_party_path.join(&crate_dir_name);

                    if dep_path.exists() {
                        let rel_path = pathdiff::diff_paths(&dep_path, cargo_toml_path.parent().unwrap())
                            .context("Failed to compute relative path")?;

                        let mut table = toml_edit::InlineTable::new();
                        table.insert(
                            "path",
                            Value::String(toml_edit::Formatted::new(
                                rel_path.to_string_lossy().to_string(),
                            )),
                        );
                        if !features.is_empty() {
                            let mut feature_array = Array::new();
                            for feature in &features {
                                feature_array.push(feature);
                            }
                            table.insert("features", Value::Array(feature_array));
                        }

                        *dep_value = Item::Value(Value::InlineTable(table));

                        println!("    Updated dependency: {} -> path = {}, features = {:?}", dep_name, rel_path.display(), features);
                    } else {
                        println!("    Skipping dependency: {} (not found in 3rd-party)", dep_name);
                    }
                } else {
                    println!("    Skipping dependency: {} (not found in metadata)", dep_name);
                }
            }
            Item::Value(Value::InlineTable(table)) => {
                // Inline table dependency
                let package_name = get_package_name_from_table(table, dep_name.get());
                let package_info = find_package_for_dependency(metadata, dep_name.get(), package_name.as_deref());

                if let Some((package, features)) = package_info {
                    let crate_dir_name = format!("{}-{}", package.name, package.version);
                    let dep_path = third_party_path.join(&crate_dir_name);

                    if dep_path.exists() {
                        let rel_path = pathdiff::diff_paths(&dep_path, cargo_toml_path.parent().unwrap())
                            .context("Failed to compute relative path")?;

                        // Remove external source fields
                        table.remove("version");
                        table.remove("git");
                        table.remove("branch");
                        table.remove("tag");
                        table.remove("rev");
                        table.remove("registry");

                        // Add path
                        table.insert(
                            "path",
                            Value::String(toml_edit::Formatted::new(
                                rel_path.to_string_lossy().to_string(),
                            )),
                        );

                        // Add features if any
                        if !features.is_empty() {
                            let mut feature_array = Array::new();
                            for feature in &features {
                                feature_array.push(feature);
                            }
                            table.insert("features", Value::Array(feature_array));
                        }

                        println!("    Updated dependency: {} -> path = {}, features = {:?}", dep_name, rel_path.display(), features);
                    } else {
                        println!("    Skipping dependency: {} (not found in 3rd-party)", dep_name);
                    }
                } else {
                    println!("    Skipping dependency: {} (not found in metadata)", dep_name);
                }
            }
            Item::Table(table) => {
                // Full table dependency
                let package_name = get_package_name_from_table_item(table, dep_name.get());
                let package_info = find_package_for_dependency(metadata, dep_name.get(), package_name.as_deref());

                if let Some((package, features)) = package_info {
                    let crate_dir_name = format!("{}-{}", package.name, package.version);
                    let dep_path = third_party_path.join(&crate_dir_name);

                    if dep_path.exists() {
                        let rel_path = pathdiff::diff_paths(&dep_path, cargo_toml_path.parent().unwrap())
                            .context("Failed to compute relative path")?;

                        // Remove external source fields
                        table.remove("version");
                        table.remove("git");
                        table.remove("branch");
                        table.remove("tag");
                        table.remove("rev");
                        table.remove("registry");

                        // Add path
                        table.insert(
                            "path",
                            Item::Value(Value::String(toml_edit::Formatted::new(
                                rel_path.to_string_lossy().to_string(),
                            ))),
                        );

                        // Add features if any
                        if !features.is_empty() {
                            let mut feature_array = Array::new();
                            for feature in &features {
                                feature_array.push(feature);
                            }
                            table.insert("features", Item::Value(Value::Array(feature_array)));
                        }

                        println!("    Updated dependency: {} -> path = {}, features = {:?}", dep_name, rel_path.display(), features);
                    } else {
                        println!("    Skipping dependency: {} (not found in 3rd-party)", dep_name);
                    }
                } else {
                    println!("    Skipping dependency: {} (not found in metadata)", dep_name);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn find_package_for_dependency<'a>(
    metadata: &'a Metadata,
    dep_name: &'a str,
    package_name: Option<&'a str>,
) -> Option<(&'a cargo_metadata::Package, Vec<String>)> {
    let resolve = metadata.resolve.as_ref()?;
    let package_map: HashMap<PackageId, &cargo_metadata::Package> = metadata
        .packages
        .iter()
        .map(|p| (p.id.clone(), p))
        .collect();

    // Find the package in the resolved dependency graph
    for node in &resolve.nodes {
        let package = package_map.get(&node.id)?;
        let actual_name = package_name.unwrap_or(dep_name);
        if package.name == actual_name {
            return Some((package, node.features.clone()));
        }
    }

    None
}

fn get_package_name_from_table(table: &toml_edit::InlineTable, _dep_name: &str) -> Option<String> {
    table.get("package")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn get_package_name_from_table_item(table: &Table, _dep_name: &str) -> Option<String> {
    table.get("package")
        .and_then(|item| item.as_str())
        .map(|s| s.to_string())
}
