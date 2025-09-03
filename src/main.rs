use anyhow::{Context, Result};
use chrono::prelude::*;
use clap::Parser;
use dirs::home_dir;
use quick_xml::events::Event;
use quick_xml::{Reader, Writer};
use regex::Regex;
use std::env;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(name = "rubymine-configurator")]
#[command(about = "Creates a Ruby interpreter configuration for RubyMine that uses shadowenv")]
struct Args {
    #[arg(long, help = "Output configuration to stdout instead of writing to RubyMine config")]
    dry_run: bool,
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
        let (ruby_wrapper_path, ruby_interpreter_path, ruby_version) = Self::detect_ruby_environment()?;
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
            println!("# Configuration file location: {}", self.interpreter_config_file()?.display());
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

        let ruby_wrapper_path = String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_string();

        if ruby_wrapper_path.is_empty() {
            anyhow::bail!("Could not find ruby in PATH");
        }

        let ruby_interpreter_path = Self::discover_actual_ruby_path(&ruby_wrapper_path)?;

        let output = Command::new("ruby")
            .arg("-e")
            .arg("puts RUBY_VERSION")
            .output()
            .context("Failed to get Ruby version")?;

        let ruby_version = String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_string();

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

    fn generate_interpreter_name(current_dir: &str, ruby_version: &str) -> String {
        let dir_name = Path::new(current_dir)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        
        let date_str = Local::now().format("%Y-%m-%d");
        format!("Ruby {} (shadowenv/{}) {}", ruby_version, dir_name, date_str)
    }

    fn rubymine_config_dir() -> Result<PathBuf> {
        let home = home_dir().context("Could not find home directory")?;

        if cfg!(target_os = "macos") {
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
                    if name_str.to_lowercase().starts_with("rubymine") && 
                       name_str.chars().any(|c| c.is_ascii_digit()) {
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
        } else if cfg!(target_os = "linux") {
            // Linux
            let config_home = env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join(".config"));
            
            let jetbrains_dir = config_home.join("JetBrains");
            let mut rubymine_dirs = Vec::new();

            if jetbrains_dir.exists() {
                for entry in fs::read_dir(&jetbrains_dir)? {
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

            // Try legacy location
            for entry in fs::read_dir(&home)? {
                let entry = entry?;
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(".RubyMine") {
                    rubymine_dirs.push(entry.path());
                }
            }
            rubymine_dirs.sort();
            rubymine_dirs.reverse();

            if let Some(dir) = rubymine_dirs.first() {
                return Ok(dir.clone());
            }
        } else if cfg!(target_os = "windows") {
            // Windows
            let appdata = env::var("APPDATA")
                .context("APPDATA environment variable not found")?;
            let jetbrains_dir = PathBuf::from(appdata).join("JetBrains");

            let mut rubymine_dirs = Vec::new();
            if jetbrains_dir.exists() {
                for entry in fs::read_dir(&jetbrains_dir)? {
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
        let mut reader = Reader::from_str(&xml_content);
        reader.config_mut().trim_text(true);
        
        let mut writer = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
        let mut buf = Vec::new();
        let mut inside_project_jdk_table = false;
        let mut skip_until_end_jdk = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    if e.name().as_ref() == b"component" {
                        let attrs: Vec<_> = e.attributes().collect();
                        for attr in &attrs {
                            if let Ok(attr) = attr {
                                if attr.key.as_ref() == b"name" && attr.value.as_ref() == b"ProjectJdkTable" {
                                    inside_project_jdk_table = true;
                                    break;
                                }
                            }
                        }
                    } else if e.name().as_ref() == b"jdk" && inside_project_jdk_table {
                        // Check if this is our interpreter by looking ahead for the name
                        let mut temp_reader = reader.clone();
                        let mut temp_buf = Vec::new();
                        let mut check_depth = 0;
                        
                        loop {
                            match temp_reader.read_event_into(&mut temp_buf) {
                                Ok(Event::Start(ref inner_e)) => {
                                    check_depth += 1;
                                    if inner_e.name().as_ref() == b"name" {
                                        let attrs: Vec<_> = inner_e.attributes().collect();
                                        for attr in &attrs {
                                            if let Ok(attr) = attr {
                                                if attr.key.as_ref() == b"value" {
                                                    let value = String::from_utf8_lossy(&attr.value);
                                                    if value == self.interpreter_name {
                                                        skip_until_end_jdk = true;
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                        break;
                                    }
                                }
                                Ok(Event::End(_)) => {
                                    check_depth -= 1;
                                    if check_depth < 0 {
                                        break;
                                    }
                                }
                                Ok(Event::Eof) => break,
                                _ => {}
                            }
                        }

                        if skip_until_end_jdk {
                            continue; // Skip writing this jdk element
                        }
                    }

                    if !skip_until_end_jdk {
                        writer.write_event(Event::Start(e.clone()))?;
                    }
                }
                Ok(Event::End(ref e)) => {
                    if e.name().as_ref() == b"jdk" && skip_until_end_jdk {
                        skip_until_end_jdk = false;
                        continue; // Skip writing the end tag too
                    } else if e.name().as_ref() == b"component" && inside_project_jdk_table {
                        // Add our interpreter before closing the component
                        self.add_shadowenv_interpreter(&mut writer)?;
                        inside_project_jdk_table = false;
                    }
                    
                    if !skip_until_end_jdk {
                        writer.write_event(Event::End(e.clone()))?;
                    }
                }
                Ok(Event::Text(e)) => {
                    if !skip_until_end_jdk {
                        writer.write_event(Event::Text(e))?;
                    }
                }
                Ok(Event::Empty(e)) => {
                    if !skip_until_end_jdk {
                        writer.write_event(Event::Empty(e))?;
                    }
                }
                Ok(Event::CData(e)) => {
                    if !skip_until_end_jdk {
                        writer.write_event(Event::CData(e))?;
                    }
                }
                Ok(Event::Comment(e)) => {
                    if !skip_until_end_jdk {
                        writer.write_event(Event::Comment(e))?;
                    }
                }
                Ok(Event::DocType(e)) => {
                    if !skip_until_end_jdk {
                        writer.write_event(Event::DocType(e))?;
                    }
                }
                Ok(Event::PI(e)) => {
                    if !skip_until_end_jdk {
                        writer.write_event(Event::PI(e))?;
                    }
                }
                Ok(Event::Decl(e)) => {
                    if !skip_until_end_jdk {
                        writer.write_event(Event::Decl(e))?;
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(anyhow::anyhow!("Error parsing XML: {}", e)),
            }
            buf.clear();
        }

        let result = writer.into_inner().into_inner();
        Ok(String::from_utf8_lossy(&result).to_string())
    }

    fn create_new_config_content(&self) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<application>
  <component name="ProjectJdkTable">
{}
  </component>
</application>"#,
            self.create_shadowenv_interpreter_xml()
        )
    }

    fn add_shadowenv_interpreter<W: std::io::Write>(&self, writer: &mut Writer<W>) -> Result<()> {
        let xml_content = self.create_shadowenv_interpreter_xml();
        let mut reader = Reader::from_str(&xml_content);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Eof) => break,
                Ok(event) => writer.write_event(event)?,
                Err(e) => return Err(anyhow::anyhow!("Error writing interpreter XML: {}", e)),
            }
            buf.clear();
        }

        Ok(())
    }

    fn create_shadowenv_interpreter_xml(&self) -> String {
        let shadowenv_path = self.find_shadowenv_path();
        
        format!(
            r#"<jdk version="2">
  <name value="{}" />
  <type value="RUBY_SDK" />
  <version value="{}" />
  <homePath value="{}" />
  <roots>
    <classPath>
      <root type="composite" />
    </classPath>
    <sourcePath>
      <root type="composite" />
    </sourcePath>
  </roots>
  <additional version="1" GEMS_BIN_DIR_PATH="{}">
    <VERSION_MANAGER ID="system">
      <custom-configurator>
        <list>
          <option value="{}" />
          <option value="exec" />
          <option value="--dir" />
          <option value="{}" />
          <option value="--" />
        </list>
      </custom-configurator>
    </VERSION_MANAGER>
  </additional>
</jdk>"#,
            self.interpreter_name,
            self.ruby_version,
            self.ruby_interpreter_path,
            Path::new(&self.ruby_interpreter_path).parent().unwrap().display(),
            shadowenv_path,
            self.current_dir
        )
    }

    fn find_shadowenv_path(&self) -> String {
        // Try to find shadowenv in PATH first
        if let Ok(output) = Command::new("which").arg("shadowenv").output() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return path;
            }
        }

        // Fallback to common locations (made portable)
        let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
        
        let common_paths = vec![
            home.join(".dev").join("userprofile").join("bin").join("shadowenv"),
            home.join(".local").join("bin").join("shadowenv"),
            PathBuf::from("/usr/local/bin/shadowenv"),
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
}

fn main() -> Result<()> {
    let args = Args::parse();
    
    let interpreter = RubyMineInterpreter::new(args.dry_run)?;
    interpreter.create_interpreter()?;

    Ok(())
}