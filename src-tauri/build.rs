use std::{
    env,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const LIBMOBI_ARCHIVE_SHA256: &str =
    "9a6fb2c56b916f8fa8b15e0c71008d908109508c944ea1d297881d4e277bf7e7";
const CONFIGURE_ARGUMENTS: &[&str] = &[
    "--disable-shared",
    "--enable-static",
    "--enable-tools-static",
    "--enable-xmlwriter",
    "--with-libxml2=no",
    "--with-zlib=no",
    "--disable-encryption",
];
const TOOLCHAIN_VARIABLES: &[&str] = &["CC", "CFLAGS", "CPPFLAGS", "LDFLAGS", "AR", "RANLIB"];
const REPRODUCIBLE_EPOCH: &str = "0";

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        io::Error::other("CARGO_MANIFEST_DIR is required to build the embedded MOBI engine")
    })?);
    let target = env::var("TARGET")?;
    let host = env::var("HOST")?;
    if !target.contains("-linux-") {
        return Err(io::Error::other(format!(
            "The embedded MOBI engine currently supports Linux only; requested target: {target}"
        ))
        .into());
    }
    if target != host {
        return Err(io::Error::other(format!(
            "Build the embedded MOBI engine on a native Linux runner for {target}; cross-compilation from {host} is not configured"
        ))
        .into());
    }
    if !target.bytes().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, b'-' | b'_' | b'.')
    }) {
        return Err(io::Error::other("The Cargo target contains invalid path characters").into());
    }
    let workspace = manifest
        .parent()
        .ok_or_else(|| io::Error::other("The desktop crate must belong to a workspace"))?;
    let source = workspace.join("vendor/libmobi-0.12").canonicalize()?;
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=build.rs");
    for variable in TOOLCHAIN_VARIABLES {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let target_directory = workspace.join("target");
    fs::create_dir_all(&target_directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(target_directory.join(format!(".libmobi-{target}.lock")))?;
    lock.lock()?;

    let build_directory = target_directory.join(format!("libmobi-{target}"));
    let engine = build_directory.join("tools/mobitool");
    let stamp = build_directory.join("library-manager-build.sha256");
    let fingerprint = source_fingerprint(&source, &manifest, &target)?;
    let epoch = env::var_os("SOURCE_DATE_EPOCH").unwrap_or_else(|| REPRODUCIBLE_EPOCH.into());
    if !cache_valid(&stamp, &engine, &fingerprint)? {
        if build_directory.exists() {
            fs::remove_dir_all(&build_directory)?;
        }
        fs::create_dir_all(&build_directory)?;
        run_logged(
            Command::new(source.join("configure"))
                .current_dir(&build_directory)
                .env("SOURCE_DATE_EPOCH", &epoch)
                .args(CONFIGURE_ARGUMENTS),
            &build_directory,
            "configure",
        )?;
        run_logged(
            Command::new("make")
                .current_dir(&build_directory)
                .env("SOURCE_DATE_EPOCH", &epoch)
                .args(["-j2"]),
            &build_directory,
            "make",
        )?;
        let engine_sha256 = hash_file(&engine)?;
        let temporary_stamp = stamp.with_extension("tmp");
        fs::write(
            &temporary_stamp,
            format!("{fingerprint}\n{engine_sha256}\n"),
        )?;
        fs::rename(temporary_stamp, &stamp)?;
    }

    let binaries = manifest.join("binaries");
    fs::create_dir_all(&binaries)?;
    let sidecar = binaries.join(format!("library-manager-mobitool-{target}"));
    let temporary_sidecar = sidecar.with_extension("tmp");
    fs::copy(&engine, &temporary_sidecar)?;
    fs::rename(temporary_sidecar, &sidecar)?;
    println!(
        "cargo:rustc-env=LIBRARY_MANAGER_MOBITOOL={}",
        sidecar.display()
    );
    drop(lock);
    tauri_build::build();
    Ok(())
}

fn source_fingerprint(source: &Path, manifest: &Path, target: &str) -> io::Result<String> {
    let mut files = Vec::new();
    collect_source_files(source, &mut files)?;
    files.push(manifest.join("build.rs"));
    files.sort();
    let hashes = file_hash_output(&files)?;
    let mut input = hashes.stdout;
    input.extend_from_slice(LIBMOBI_ARCHIVE_SHA256.as_bytes());
    input.extend_from_slice(target.as_bytes());
    let compiler = Command::new("cc").arg("--version").output()?;
    ensure_success(&compiler, "C compiler version")?;
    input.extend_from_slice(&compiler.stdout);
    input.extend_from_slice(
        env::var_os("SOURCE_DATE_EPOCH")
            .unwrap_or_else(|| REPRODUCIBLE_EPOCH.into())
            .as_encoded_bytes(),
    );
    for argument in CONFIGURE_ARGUMENTS {
        input.push(0);
        input.extend_from_slice(argument.as_bytes());
    }
    for variable in TOOLCHAIN_VARIABLES {
        input.push(0);
        input.extend_from_slice(variable.as_bytes());
        input.push(b'=');
        if let Some(value) = env::var_os(variable) {
            input.extend_from_slice(value.as_encoded_bytes());
        }
    }
    hash_input(&input)
}

fn collect_source_files(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_source_files(&entry.path(), files)?;
        } else if file_type.is_file() {
            files.push(entry.path());
        } else {
            return Err(io::Error::other(format!(
                "Unsupported file type in the vendored MOBI source: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn cache_valid(stamp: &Path, engine: &Path, fingerprint: &str) -> io::Result<bool> {
    if !stamp.is_file() || !engine.is_file() {
        return Ok(false);
    }
    let stamp_content = fs::read_to_string(stamp)?;
    let mut lines = stamp_content.lines();
    if lines.next() != Some(fingerprint) {
        return Ok(false);
    }
    let expected_engine_hash = lines.next();
    Ok(expected_engine_hash == Some(hash_file(engine)?.as_str()))
}

fn file_hash_output(files: &[PathBuf]) -> io::Result<Output> {
    let output = Command::new("sha256sum")
        .args(["--binary", "--zero", "--"])
        .args(files)
        .output()
        .map_err(|error| {
            io::Error::other(format!(
                "GNU sha256sum is required to verify the embedded MOBI build: {error}"
            ))
        })?;
    ensure_success(&output, "sha256sum")?;
    Ok(output)
}

fn hash_file(file: &Path) -> io::Result<String> {
    parse_hash(&file_hash_output(&[file.to_path_buf()])?.stdout)
}

fn hash_input(input: &[u8]) -> io::Result<String> {
    let mut process = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = process
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("Cannot open sha256sum input"))?;
    stdin.write_all(input)?;
    drop(stdin);
    let output = process.wait_with_output()?;
    ensure_success(&output, "sha256sum")?;
    parse_hash(&output.stdout)
}

fn parse_hash(output: &[u8]) -> io::Result<String> {
    let hash = String::from_utf8_lossy(output)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    if hash.len() != 64 || !hash.bytes().all(|character| character.is_ascii_hexdigit()) {
        return Err(io::Error::other("sha256sum returned an invalid digest"));
    }
    Ok(hash)
}

fn run_logged(command: &mut Command, directory: &Path, step: &str) -> io::Result<()> {
    let output = command.output().map_err(|error| {
        io::Error::other(format!(
            "Cannot run embedded MOBI build step {step}: {error}"
        ))
    })?;
    let mut log = File::create(directory.join(format!("{step}.log")))?;
    log.write_all(&output.stdout)?;
    log.write_all(&output.stderr)?;
    ensure_success(&output, step)
}

fn ensure_success(output: &Output, step: &str) -> io::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail: String = detail
        .chars()
        .rev()
        .take(3000)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    Err(io::Error::other(format!(
        "Embedded MOBI build step {step} failed ({}): {detail}",
        output.status
    )))
}
