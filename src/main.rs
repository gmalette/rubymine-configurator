use anyhow::{Context, Result};
use chrono::prelude::*;
use clap::Parser;
use dirs::home_dir;
use regex::Regex;
use roxmltree::Document;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;
use xmlwriter::{Options, XmlWriter};

#[derive(Parser)]
#[command(name = "rubymine-configurator")]
#[command(about = "Creates a Ruby interpreter configuration for RubyMine that uses shadowenv")]
struct Args {
    #[arg(
        long,
        help = "Output configuration to stdout instead of writing to RubyMine config"
    )]
    dry_run: bool,
}

#[derive(Debug)]
struct MySqlConfig {
    host: String,
    port: String,
    user: String,
    password: String,
}

struct RubyMineInterpreter {
    ruby_wrapper_path: String,
    ruby_interpreter_path: String,
    ruby_version: String,
    interpreter_name: String,
    current_dir: String,
    dry_run: bool,
}

impl RubyMineInterpreter {
    fn new(dry_run: bool) -> Result<Self> {
        let current_dir = env::current_dir()?.to_string_lossy().to_string();
        let (ruby_wrapper_path, ruby_interpreter_path, ruby_version) =
            Self::detect_ruby_environment()?;
        let interpreter_name = Self::generate_interpreter_name(&current_dir, &ruby_version);

        Ok(Self {
            ruby_wrapper_path,
            ruby_interpreter_path,
            ruby_version,
            interpreter_name,
            current_dir,
            dry_run,
        })
    }

    fn create_interpreter(&self) -> Result<()> {
        if self.dry_run {
            println!(
                "# Configuration file location: {}",
                self.interpreter_config_file()?.display()
            );
            println!("# Interpreter name: {}", self.interpreter_name);
            println!("# Ruby wrapper: {}", self.ruby_wrapper_path);
            println!("# Ruby interpreter: {}", self.ruby_interpreter_path);
            println!("# Ruby version: {}", self.ruby_version);
            println!("# Current directory: {}", self.current_dir);
            println!("# {}", "=".repeat(50));
            println!();
        } else {
            self.ensure_rubymine_config_exists()?;
            println!("Creating RubyMine interpreter: {}", self.interpreter_name);
            println!("Ruby wrapper: {}", self.ruby_wrapper_path);
            println!("Ruby interpreter: {}", self.ruby_interpreter_path);
            println!("Ruby version: {}", self.ruby_version);
            println!("Current directory: {}", self.current_dir);
            println!("Config file: {}", self.interpreter_config_file()?.display());
        }

        let config_content = self.create_interpreter_config()?;

        if self.dry_run {
            println!("{}", config_content);
        } else {
            self.write_config_file(&config_content)?;
            println!("Interpreter created successfully!");
            println!("Restart RubyMine to see the new interpreter in Project Settings > Project Interpreter");
        }

        Ok(())
    }

    fn detect_ruby_environment() -> Result<(String, String, String)> {
        let output = Command::new("which")
            .arg("ruby")
            .output()
            .context("Failed to execute 'which ruby'")?;

        let ruby_wrapper_path = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if ruby_wrapper_path.is_empty() {
            anyhow::bail!("Could not find ruby in PATH");
        }

        let ruby_interpreter_path = Self::discover_actual_ruby_path(&ruby_wrapper_path)?;

        let output = Command::new("ruby")
            .arg("-e")
            .arg("puts RUBY_VERSION")
            .output()
            .context("Failed to get Ruby version")?;

        let ruby_version = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if ruby_version.is_empty() {
            anyhow::bail!("Could not determine Ruby version");
        }

        Ok((ruby_wrapper_path, ruby_interpreter_path, ruby_version))
    }

    fn discover_actual_ruby_path(ruby_wrapper_path: &str) -> Result<String> {
        if Path::new(ruby_wrapper_path).exists() {
            let content = match fs::read_to_string(ruby_wrapper_path) {
                Ok(content) => content,
                Err(_) => {
                    // If we can't read as UTF-8, try reading as bytes and convert lossy
                    let bytes = fs::read(ruby_wrapper_path)?;
                    String::from_utf8_lossy(&bytes).to_string()
                }
            };

            // Look for exec line with actual ruby path
            let re1 = Regex::new(r#"exec\s+"([^"]+)""#)?;
            if let Some(captures) = re1.captures(&content) {
                return Ok(captures[1].to_string());
            }

            let re2 = Regex::new(r"exec\s+([^\s]+)")?;
            if let Some(captures) = re2.captures(&content) {
                return Ok(captures[1].to_string());
            }
        }

        // Fallback to which ruby result
        Ok(ruby_wrapper_path.to_string())
    }

    fn extract_worktree_name(current_dir: &str) -> String {
        let path = Path::new(current_dir);
        let path_str = path.to_string_lossy();

        // Look for patterns like /trees/{worktree}/src or /trees/{worktree}
        if let Some(trees_pos) = path_str.find("/trees/") {
            let after_trees = &path_str[trees_pos + 7..]; // Skip "/trees/"
            if let Some(slash_pos) = after_trees.find('/') {
                return after_trees[..slash_pos].to_string();
            } else {
                return after_trees.to_string();
            }
        }

        // Fallback to directory name
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    fn generate_interpreter_name(current_dir: &str, ruby_version: &str) -> String {
        let current_dir_name = Path::new(current_dir)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");

        let path_str = Path::new(current_dir).to_string_lossy();
        let name_part = if let Some(trees_pos) = path_str.find("/trees/") {
            let after_trees = &path_str[trees_pos + 7..]; // Skip "/trees/"
            if let Some(slash_pos) = after_trees.find('/') {
                let worktree_name = &after_trees[..slash_pos];
                format!("{}/{}", worktree_name, current_dir_name)
            } else {
                // Just the worktree name, no subdirectory
                format!("{}/{}", after_trees, current_dir_name)
            }
        } else {
            current_dir_name.to_string()
        };

        let date_str = Local::now().format("%Y-%m-%d");
        format!(
            "Ruby {} ({}) + shadowenv {}",
            ruby_version, name_part, date_str
        )
    }

    fn is_same_worktree_interpreter(&self, interpreter_name: &str) -> bool {
        let current_worktree = Self::extract_worktree_name(&self.current_dir);

        // Check if the interpreter name matches the pattern for the same worktree
        // Pattern: "Ruby {version} ({worktree}/{current_dir}) + shadowenv {date}"

        if let Some(start) = interpreter_name.find('(') {
            if let Some(end) = interpreter_name[start..].find(')') {
                let path_part = &interpreter_name[start + 1..start + end]; // Skip "("

                // Check if it contains a slash (worktree format)
                if let Some(slash_pos) = path_part.find('/') {
                    let worktree_part = &path_part[..slash_pos];
                    return worktree_part == current_worktree;
                } else {
                    // No slash, compare with current directory name if no worktree
                    let current_dir_name = Path::new(&self.current_dir)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("unknown");
                    return path_part == current_dir_name && current_worktree == current_dir_name;
                }
            }
        }

        false
    }

    fn rubymine_config_dir() -> Result<PathBuf> {
        let home = home_dir().context("Could not find home directory")?;

        // macOS - check Application Support first (newer location)
        let app_support = home.join("Library").join("Application Support");
        let jetbrains_dir = app_support.join("JetBrains");

        // Look for versioned RubyMine directories
        if jetbrains_dir.exists() {
            let mut rubymine_dirs = Vec::new();
            for entry in fs::read_dir(&jetbrains_dir)? {
                let entry = entry?;
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.to_lowercase().starts_with("rubymine")
                    && name_str.chars().any(|c| c.is_ascii_digit())
                {
                    rubymine_dirs.push(entry.path());
                }
            }

            // Sort by modification time to get the most recent
            rubymine_dirs.sort_by_key(|path| {
                fs::metadata(path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH)
            });
            rubymine_dirs.reverse(); // Most recent first

            if let Some(dir) = rubymine_dirs.first() {
                return Ok(dir.clone());
            }
        }

        // Try Library/Preferences as fallback (older location)
        let library_prefs = home.join("Library").join("Preferences");
        let mut rubymine_dirs = Vec::new();
        if library_prefs.exists() {
            for entry in fs::read_dir(&library_prefs)? {
                let entry = entry?;
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("RubyMine") {
                    rubymine_dirs.push(entry.path());
                }
            }
            rubymine_dirs.sort();
            rubymine_dirs.reverse();

            if let Some(dir) = rubymine_dirs.first() {
                return Ok(dir.clone());
            }
        }

        anyhow::bail!("No RubyMine configuration directory found");
    }

    fn options_dir(&self) -> Result<PathBuf> {
        Ok(Self::rubymine_config_dir()?.join("options"))
    }

    fn interpreter_config_file(&self) -> Result<PathBuf> {
        Ok(self.options_dir()?.join("jdk.table.xml"))
    }

    fn ensure_rubymine_config_exists(&self) -> Result<()> {
        let options_dir = self.options_dir()?;
        if !options_dir.exists() {
            fs::create_dir_all(&options_dir)?;
        }
        Ok(())
    }

    fn create_interpreter_config(&self) -> Result<String> {
        let config_file = self.interpreter_config_file()?;
        if config_file.exists() {
            self.update_existing_config(&config_file)
        } else {
            Ok(self.create_new_config_content())
        }
    }

    fn write_config_file(&self, content: &str) -> Result<()> {
        let config_file = self.interpreter_config_file()?;

        // Create backup if file exists
        if config_file.exists() {
            let timestamp = Local::now().format("%Y%m%d_%H%M%S");
            let backup_file = config_file.with_extension(format!("backup.{}.xml", timestamp));
            fs::copy(&config_file, &backup_file)?;
            println!("Backup created: {}", backup_file.display());
        }

        fs::write(&config_file, content)?;
        Ok(())
    }

    fn update_existing_config(&self, config_file: &Path) -> Result<String> {
        let xml_content = fs::read_to_string(config_file)?;
        let doc = Document::parse(&xml_content)?;

        let mut writer = XmlWriter::new(Options::default());
        writer.write_declaration();

        // Find the root element
        let root = doc.root_element();
        self.write_element_with_interpreter(&mut writer, &root)?;

        Ok(writer.end_document())
    }

    fn write_element_with_interpreter(
        &self,
        writer: &mut XmlWriter,
        node: &roxmltree::Node,
    ) -> Result<()> {
        if node.is_element() {
            let tag_name = node.tag_name().name();
            writer.start_element(tag_name);

            // Write attributes
            for attr in node.attributes() {
                writer.write_attribute(attr.name(), attr.value());
            }

            // Check if this is the ProjectJdkTable component
            let is_project_jdk_table =
                tag_name == "component" && node.attribute("name") == Some("ProjectJdkTable");

            // Write child elements
            for child in node.children() {
                if child.is_element() {
                    // Skip existing interpreters for the same worktree
                    if is_project_jdk_table && child.tag_name().name() == "jdk" {
                        if let Some(name_node) = child.descendants().find(|n| {
                            n.tag_name().name() == "name" && n.attribute("value").is_some()
                        }) {
                            if let Some(name_value) = name_node.attribute("value") {
                                if self.is_same_worktree_interpreter(name_value) {
                                    continue; // Skip this JDK
                                }
                            }
                        }
                    }
                    self.write_element_with_interpreter(writer, &child)?;
                } else if child.is_text() {
                    if let Some(text) = child.text() {
                        if !text.trim().is_empty() {
                            writer.write_text(text);
                        }
                    }
                }
            }

            // Add our interpreter before closing ProjectJdkTable component
            if is_project_jdk_table {
                self.write_shadowenv_interpreter(writer)?;
            }

            writer.end_element();
        }
        Ok(())
    }

    fn create_new_config_content(&self) -> String {
        let mut writer = XmlWriter::new(Options::default());
        writer.write_declaration();
        writer.start_element("application");
        writer.start_element("component");
        writer.write_attribute("name", "ProjectJdkTable");
        self.write_shadowenv_interpreter(&mut writer).unwrap();
        writer.end_element(); // component
        writer.end_element(); // application
        writer.end_document()
    }

    fn write_shadowenv_interpreter(&self, writer: &mut XmlWriter) -> Result<()> {
        let shadowenv_path = self.find_shadowenv_path();
        let gems_bin_dir = Path::new(&self.ruby_interpreter_path)
            .parent()
            .unwrap()
            .display()
            .to_string();

        writer.start_element("jdk");
        writer.write_attribute("version", "2");

        writer.start_element("name");
        writer.write_attribute("value", &self.interpreter_name);
        writer.end_element();

        writer.start_element("type");
        writer.write_attribute("value", "RUBY_SDK");
        writer.end_element();

        writer.start_element("version");
        writer.write_attribute("value", &self.ruby_version);
        writer.end_element();

        writer.start_element("homePath");
        writer.write_attribute("value", &self.ruby_interpreter_path);
        writer.end_element();

        // roots
        writer.start_element("roots");

        writer.start_element("classPath");
        writer.start_element("root");
        writer.write_attribute("type", "composite");
        writer.end_element();
        writer.end_element(); // classPath

        writer.start_element("sourcePath");
        writer.start_element("root");
        writer.write_attribute("type", "composite");
        writer.end_element();
        writer.end_element(); // sourcePath

        writer.end_element(); // roots

        // additional
        writer.start_element("additional");
        writer.write_attribute("version", "1");
        writer.write_attribute("GEMS_BIN_DIR_PATH", &gems_bin_dir);

        writer.start_element("VERSION_MANAGER");
        writer.write_attribute("ID", "system");

        writer.start_element("custom-configurator");
        writer.start_element("list");

        writer.start_element("option");
        writer.write_attribute("value", &shadowenv_path);
        writer.end_element();

        writer.start_element("option");
        writer.write_attribute("value", "exec");
        writer.end_element();

        writer.start_element("option");
        writer.write_attribute("value", "--dir");
        writer.end_element();

        writer.start_element("option");
        writer.write_attribute("value", &self.current_dir);
        writer.end_element();

        writer.start_element("option");
        writer.write_attribute("value", "--");
        writer.end_element();

        writer.end_element(); // list
        writer.end_element(); // custom-configurator
        writer.end_element(); // VERSION_MANAGER
        writer.end_element(); // additional
        writer.end_element(); // jdk

        Ok(())
    }

    fn find_shadowenv_path(&self) -> String {
        // Check homebrew first (Apple Silicon)
        let homebrew_path = PathBuf::from("/opt/homebrew/bin/shadowenv");
        if homebrew_path.exists() {
            return homebrew_path.to_string_lossy().to_string();
        }

        // Then try PATH
        if let Ok(output) = Command::new("which").arg("shadowenv").output() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return path;
            }
        }

        // Fallback to other common locations
        let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));

        let common_paths = vec![
            home.join(".dev")
                .join("userprofile")
                .join("bin")
                .join("shadowenv"),
            home.join(".local").join("bin").join("shadowenv"),
            PathBuf::from("/opt/dev/bin/shadowenv"),
        ];

        for path in common_paths {
            if path.exists() {
                return path.to_string_lossy().to_string();
            }
        }

        // Last resort fallback
        "shadowenv".to_string()
    }

    fn find_rubymine_app_path() -> Result<PathBuf> {
        // Check user Applications first
        if let Some(home) = home_dir() {
            let user_app = home.join("Applications/RubyMine.app");
            if user_app.exists() {
                return Ok(user_app);
            }
        }

        // Check system Applications
        let system_app = PathBuf::from("/Applications/RubyMine.app");
        if system_app.exists() {
            return Ok(system_app);
        }

        anyhow::bail!("RubyMine.app not found in ~/Applications or /Applications")
    }

    fn find_workspace_files(&self) -> Result<Vec<PathBuf>> {
        let mut workspace_files = Vec::new();

        // 1. Check for project-specific .idea/workspace.xml
        let project_workspace = Path::new(&self.current_dir).join(".idea/workspace.xml");
        if project_workspace.exists() {
            workspace_files.push(project_workspace);
        }

        // 2. Find global workspace files in RubyMine config directories
        let rubymine_config_dir = Self::rubymine_config_dir()?;
        let workspace_dir = rubymine_config_dir.join("workspace");

        if workspace_dir.exists() {
            for entry in fs::read_dir(&workspace_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("xml") {
                    // Check if this workspace file contains our project
                    if self.workspace_contains_project(&path)? {
                        workspace_files.push(path);
                    }
                }
            }
        }

        Ok(workspace_files)
    }

    fn workspace_contains_project(&self, workspace_file: &Path) -> Result<bool> {
        let content = fs::read_to_string(workspace_file)?;
        let current_path = Path::new(&self.current_dir);
        let current_name = current_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");

        // Look for project references in the workspace XML
        // This is a simple heuristic - could be made more robust
        Ok(content.contains(&self.current_dir)
            || content.contains(&format!("$PROJECT_DIR$"))
            || content.contains(current_name))
    }

    fn create_minitest_config(&self) -> Result<()> {
        let rubymine_app_path = Self::find_rubymine_app_path()?;
        let workspace_files = self.find_workspace_files()?;

        if workspace_files.is_empty() {
            if self.dry_run {
                println!("# No workspace files found for the current project");
            } else {
                println!("No workspace files found for the current project");
            }
            return Ok(());
        }

        let ruby_args = self.generate_ruby_args(&rubymine_app_path);

        if self.dry_run {
            println!("# Minitest Configuration Updates:");
            println!("# RubyMine app path: {}", rubymine_app_path.display());
            println!("# Updated RUBY_ARGS: {}", ruby_args);
            println!("# {}", "=".repeat(50));
            println!();
        } else {
            println!("Updating Minitest configuration...");
            println!("RubyMine app path: {}", rubymine_app_path.display());
        }

        for workspace_file in &workspace_files {
            if self.dry_run {
                println!("# Workspace file: {}", workspace_file.display());

                // Show what the updated configuration would look like
                if let Ok(content) =
                    self.preview_minitest_config_changes(workspace_file, &ruby_args)
                {
                    println!("{}", content);
                } else {
                    println!("# Unable to preview changes for this file");
                }
                println!();
            } else {
                println!("Updating: {}", workspace_file.display());
                self.update_workspace_minitest_config(workspace_file, &ruby_args)?;
            }
        }

        if !self.dry_run {
            println!("Minitest configuration updated successfully!");
            println!("Restart RubyMine to see the updated test template configuration");
        }

        Ok(())
    }

    fn generate_ruby_args(&self, rubymine_app_path: &Path) -> String {
        let plugin_path = rubymine_app_path.join("Contents/plugins/ruby/rb/testing/patch");

        vec![
            plugin_path.join("common"),
            plugin_path.join("bdd"),
            plugin_path.join("rake"),
            plugin_path.join("testunit"),
        ]
        .iter()
        .map(|path| format!("-I{}", path.display()))
        .collect::<Vec<_>>()
        .join(" ")
    }

    fn update_workspace_minitest_config(
        &self,
        workspace_file: &Path,
        ruby_args: &str,
    ) -> Result<()> {
        let xml_content = fs::read_to_string(workspace_file)?;
        let doc = Document::parse(&xml_content)?;

        let mut updated = false;
        let mut writer = XmlWriter::new(Options::default());
        writer.write_declaration();

        let root = doc.root_element();
        self.write_workspace_element(&mut writer, &root, ruby_args, &mut updated)?;

        if updated {
            // Create backup
            if workspace_file.exists() {
                let timestamp = Local::now().format("%Y%m%d_%H%M%S");
                let backup_file =
                    workspace_file.with_extension(format!("backup.{}.xml", timestamp));
                fs::copy(workspace_file, &backup_file)?;
                println!("Backup created: {}", backup_file.display());
            }

            // Write updated content
            fs::write(workspace_file, writer.end_document())?;
        }

        Ok(())
    }

    fn write_workspace_element(
        &self,
        writer: &mut XmlWriter,
        node: &roxmltree::Node,
        ruby_args: &str,
        updated: &mut bool,
    ) -> Result<()> {
        if node.is_element() {
            let tag_name = node.tag_name().name();
            writer.start_element(tag_name);

            // Write attributes, updating RUBY_ARGS if necessary
            for attr in node.attributes() {
                if tag_name == "RTEST_RUN_CONFIG_SETTINGS_ID"
                    && attr.name() == "NAME"
                    && attr.value() == "RUBY_ARGS"
                {
                    // This is a RUBY_ARGS element, update the VALUE attribute
                    writer.write_attribute("NAME", "RUBY_ARGS");
                    writer.write_attribute("VALUE", ruby_args);
                    *updated = true;

                    // Skip the original VALUE attribute
                    for other_attr in node.attributes() {
                        if other_attr.name() != "NAME" && other_attr.name() != "VALUE" {
                            writer.write_attribute(other_attr.name(), other_attr.value());
                        }
                    }
                    writer.end_element();
                    return Ok(());
                } else {
                    writer.write_attribute(attr.name(), attr.value());
                }
            }

            // Write child elements
            for child in node.children() {
                if child.is_element() {
                    self.write_workspace_element(writer, &child, ruby_args, updated)?;
                } else if child.is_text() {
                    if let Some(text) = child.text() {
                        if !text.trim().is_empty() {
                            writer.write_text(text);
                        }
                    }
                }
            }

            writer.end_element();
        }
        Ok(())
    }

    fn preview_minitest_config_changes(
        &self,
        workspace_file: &Path,
        ruby_args: &str,
    ) -> Result<String> {
        let xml_content = fs::read_to_string(workspace_file)?;
        let doc = Document::parse(&xml_content)?;

        // Check if there are any Minitest configurations
        let has_minitest_config = doc.descendants().any(|node| {
            node.tag_name().name() == "configuration"
                && node.attribute("type") == Some("TestUnitRunConfigurationType")
        });

        if !has_minitest_config {
            return Ok("# No Minitest configurations found in this workspace file".to_string());
        }

        let mut updated = false;
        let mut writer = XmlWriter::new(Options::default());
        writer.write_declaration();

        let root = doc.root_element();
        self.write_workspace_element(&mut writer, &root, ruby_args, &mut updated)?;

        Ok(writer.end_document())
    }

    fn read_mysql_config() -> Option<MySqlConfig> {
        let host = env::var("MYSQL_HOST").ok()?;
        let port = env::var("MYSQL_PORT").ok()?;
        let user = env::var("MYSQL_USER").ok()?;
        let password = env::var("MYSQL_PASSWORD").unwrap_or_default();

        Some(MySqlConfig {
            host,
            port,
            user,
            password,
        })
    }

    fn idea_dir(&self) -> PathBuf {
        Path::new(&self.current_dir).join(".idea")
    }

    fn datasources_xml_path(&self) -> PathBuf {
        self.idea_dir().join("dataSources.xml")
    }

    fn datasources_local_xml_path(&self) -> PathBuf {
        self.idea_dir().join("dataSources.local.xml")
    }

    fn get_or_generate_datasource_uuid(&self) -> Result<String> {
        let datasources_path = self.datasources_xml_path();

        if datasources_path.exists() {
            // Try to read existing UUID
            let content = fs::read_to_string(&datasources_path)?;
            let doc = Document::parse(&content)?;

            // Look for existing data-source element with uuid attribute
            for node in doc.descendants() {
                if node.tag_name().name() == "data-source" {
                    if let Some(uuid) = node.attribute("uuid") {
                        return Ok(uuid.to_string());
                    }
                }
            }
        }

        // Generate new UUID if file doesn't exist or no UUID found
        Ok(Uuid::new_v4().to_string())
    }

    fn create_datasources_xml(&self, mysql_config: &MySqlConfig, uuid: &str) -> String {
        let mut writer = XmlWriter::new(Options::default());
        writer.write_declaration();

        writer.start_element("project");
        writer.write_attribute("version", "4");

        writer.start_element("component");
        writer.write_attribute("name", "DataSourceManagerImpl");
        writer.write_attribute("format", "xml");
        writer.write_attribute("multifile-model", "true");

        writer.start_element("data-source");
        writer.write_attribute("source", "LOCAL");
        writer.write_attribute("name", &format!("@{}", mysql_config.host));
        writer.write_attribute("uuid", uuid);

        writer.start_element("driver-ref");
        writer.write_text("mysql.8");
        writer.end_element();

        writer.start_element("synchronize");
        writer.write_text("true");
        writer.end_element();

        writer.start_element("jdbc-driver");
        writer.write_text("com.mysql.cj.jdbc.Driver");
        writer.end_element();

        writer.start_element("jdbc-url");
        writer.write_text(&format!(
            "jdbc:mysql://{}:{}",
            mysql_config.host, mysql_config.port
        ));
        writer.end_element();

        writer.start_element("jdbc-additional-properties");

        writer.start_element("property");
        writer.write_attribute("name", "com.intellij.clouds.kubernetes.db.enabled");
        writer.write_attribute("value", "false");
        writer.end_element();

        writer.end_element(); // jdbc-additional-properties

        writer.start_element("working-dir");
        writer.write_text("$ProjectFileDir$");
        writer.end_element();

        writer.end_element(); // data-source
        writer.end_element(); // component
        writer.end_element(); // project

        writer.end_document()
    }

    fn create_datasources_local_xml(&self, mysql_config: &MySqlConfig, uuid: &str) -> String {
        let mut writer = XmlWriter::new(Options::default());
        writer.write_declaration();

        writer.start_element("project");
        writer.write_attribute("version", "4");

        writer.start_element("component");
        writer.write_attribute("name", "dataSourceStorageLocal");
        writer.write_attribute("created-in", "RM-233.15026.15");

        writer.start_element("data-source");
        writer.write_attribute("name", &format!("@{}", mysql_config.host));
        writer.write_attribute("uuid", uuid);

        writer.start_element("database-info");
        writer.write_attribute("product", "MySQL");
        writer.write_attribute("version", "8.0.11");
        writer.write_attribute("jdbc-version", "4.2");
        writer.write_attribute("driver-name", "MySQL Connector/J");
        writer.write_attribute(
            "driver-version",
            "mysql-connector-java-8.0.25 (Revision: 08be9e9b4cba6aa115f9b27b215887af40b159e0)",
        );
        writer.write_attribute("dbms", "MYSQL");
        writer.write_attribute("exact-version", "8.0.11");
        writer.write_attribute("exact-driver-version", "8.0");

        writer.start_element("extra-name-characters");
        writer.write_text("#@");
        writer.end_element();

        writer.start_element("identifier-quote-string");
        writer.write_text("`");
        writer.end_element();

        writer.end_element(); // database-info

        writer.start_element("case-sensitivity");
        writer.write_attribute("plain-identifiers", "lower");
        writer.write_attribute("quoted-identifiers", "lower");
        writer.end_element();

        writer.start_element("secret-storage");
        writer.write_text("master_key");
        writer.end_element();

        writer.start_element("user-name");
        writer.write_text(&mysql_config.user);
        writer.end_element();

        writer.start_element("schema-mapping");
        writer.start_element("introspection-scope");

        let schemas = vec![
            "@",
            "storefront_renderer_test_master",
            "storefront_renderer_test_shard",
            "storefront_renderer_dev_shard",
        ];

        for schema in schemas {
            writer.start_element("node");
            writer.write_attribute("kind", "schema");
            writer.write_attribute("qname", schema);
            writer.end_element();
        }

        writer.end_element(); // introspection-scope
        writer.end_element(); // schema-mapping

        writer.end_element(); // data-source
        writer.end_element(); // component
        writer.end_element(); // project

        writer.end_document()
    }

    fn configure_datasources(&self) -> Result<()> {
        let mysql_config = match Self::read_mysql_config() {
            Some(config) => config,
            None => {
                if self.dry_run {
                    println!("# MySQL environment variables not found, skipping datasource configuration");
                } else {
                    println!(
                        "MySQL environment variables not found, skipping datasource configuration"
                    );
                }
                return Ok(());
            }
        };

        if self.dry_run {
            println!("# MySQL Configuration:");
            println!("# Host: {}", mysql_config.host);
            println!("# Port: {}", mysql_config.port);
            println!("# User: {}", mysql_config.user);
            println!(
                "# Password: {}",
                if mysql_config.password.is_empty() {
                    "(empty)"
                } else {
                    "(set)"
                }
            );
            println!("# {}", "=".repeat(50));
            println!();
        } else {
            println!("Configuring MySQL datasources...");
            println!("Host: {}", mysql_config.host);
            println!("Port: {}", mysql_config.port);
            println!("User: {}", mysql_config.user);
        }

        let uuid = self.get_or_generate_datasource_uuid()?;

        let datasources_xml = self.create_datasources_xml(&mysql_config, &uuid);
        let datasources_local_xml = self.create_datasources_local_xml(&mysql_config, &uuid);

        if self.dry_run {
            println!("# dataSources.xml:");
            println!("{}", datasources_xml);
            println!();
            println!("# dataSources.local.xml:");
            println!("{}", datasources_local_xml);
        } else {
            // Ensure .idea directory exists
            let idea_dir = self.idea_dir();
            if !idea_dir.exists() {
                fs::create_dir_all(&idea_dir)?;
            }

            // Write dataSources.xml
            let datasources_path = self.datasources_xml_path();
            if datasources_path.exists() {
                let timestamp = Local::now().format("%Y%m%d_%H%M%S");
                let backup_file =
                    datasources_path.with_extension(format!("backup.{}.xml", timestamp));
                fs::copy(&datasources_path, &backup_file)?;
                println!("Backup created: {}", backup_file.display());
            }
            fs::write(&datasources_path, datasources_xml)?;
            println!("Created: {}", datasources_path.display());

            // Write dataSources.local.xml
            let datasources_local_path = self.datasources_local_xml_path();
            if datasources_local_path.exists() {
                let timestamp = Local::now().format("%Y%m%d_%H%M%S");
                let backup_file =
                    datasources_local_path.with_extension(format!("backup.{}.xml", timestamp));
                fs::copy(&datasources_local_path, &backup_file)?;
                println!("Backup created: {}", backup_file.display());
            }
            fs::write(&datasources_local_path, datasources_local_xml)?;
            println!("Created: {}", datasources_local_path.display());

            println!("Datasource configuration completed successfully!");
        }

        Ok(())
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    let interpreter = RubyMineInterpreter::new(args.dry_run)?;
    interpreter.create_interpreter()?;
    interpreter.create_minitest_config()?;
    interpreter.configure_datasources()?;

    Ok(())
}
