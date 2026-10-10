use sha2::{Digest, Sha256};
use std::{
    env,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
};

const LIBMOBI_ARCHIVE_SHA256: &str =
    "9a6fb2c56b916f8fa8b15e0c71008d908109508c944ea1d297881d4e277bf7e7";
const CONFIGURE_ARGUMENTS: &[&str] = &[
    "--disable-shared",
    "--enable-static",
    "--enable-tools-static",
    "--enable-xmlwriter",
    "--with-libxml2=no",
    "--disable-encryption",
];
const TOOLCHAIN_VARIABLES: &[&str] = &[
    "CC",
    "CFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "AR",
    "RANLIB",
    "MACOSX_DEPLOYMENT_TARGET",
    "LIBRARY_MANAGER_MSYS2_ROOT",
];
const REPRODUCIBLE_EPOCH: &str = "0";

enum Toolchain {
    Unix,
    Windows { root: PathBuf },
}

impl Toolchain {
    fn native(target: &str, host: &str) -> io::Result<Self> {
        if target != host {
            return Err(io::Error::other(format!(
                "Build the embedded MOBI engine on a native runner for {target}; cross-compilation from {host} is not configured"
            )));
        }
        if target.contains("-linux-") || target.ends_with("-apple-darwin") {
            return Ok(Self::Unix);
        }
        if target == "x86_64-pc-windows-msvc" {
            let root = PathBuf::from(env::var_os("LIBRARY_MANAGER_MSYS2_ROOT").ok_or_else(|| {
                io::Error::other("LIBRARY_MANAGER_MSYS2_ROOT must name the native MSYS2 installation containing UCRT64 GCC, make and static zlib")
            })?);
            if !root.is_absolute() {
                return Err(io::Error::other(
                    "LIBRARY_MANAGER_MSYS2_ROOT must be an absolute Windows path",
                ));
            }
            for required in [
                "usr/bin/bash.exe",
                "usr/bin/cygpath.exe",
                "usr/bin/make.exe",
                "ucrt64/bin/gcc.exe",
                "ucrt64/bin/ar.exe",
                "ucrt64/bin/ranlib.exe",
                "ucrt64/bin/objdump.exe",
                "ucrt64/include/zlib.h",
                "ucrt64/lib/libz.a",
            ] {
                if !root.join(required).is_file() {
                    return Err(io::Error::other(format!(
                        "The native MSYS2 UCRT64 toolchain is incomplete: missing {required}"
                    )));
                }
            }
            return Ok(Self::Windows { root });
        }
        Err(io::Error::other(format!(
            "The embedded MOBI engine supports native Linux, macOS and x86_64 Windows/MSVC runners; requested target: {target}"
        )))
    }

    fn executable_suffix(&self) -> &'static str {
        match self {
            Self::Unix => "",
            Self::Windows { .. } => ".exe",
        }
    }

    fn compiler_version(&self) -> io::Result<Output> {
        let output = match self {
            // Autotools permits a compiler launcher and arguments in CC, such as ccache clang.
            // Expansion is intentional; the variable is never evaluated as shell source.
            Self::Unix => Command::new("sh")
                .args(["-c", "exec ${CC:-cc} --version"])
                .env("LC_ALL", "C")
                .output()?,
            Self::Windows { root } => Command::new(root.join("ucrt64/bin/gcc.exe"))
                .arg("--version")
                .env("LC_ALL", "C")
                .output()?,
        };
        ensure_success(&output, "C compiler version")?;
        Ok(output)
    }

    fn build(&self, source: &Path, directory: &Path) -> io::Result<()> {
        let epoch = env::var_os("SOURCE_DATE_EPOCH").unwrap_or_else(|| REPRODUCIBLE_EPOCH.into());
        match self {
            Self::Unix => {
                run_logged(
                    Command::new(source.join("configure"))
                        .current_dir(directory)
                        .env("SOURCE_DATE_EPOCH", &epoch)
                        .args(CONFIGURE_ARGUMENTS)
                        .arg("--with-zlib=no"),
                    directory,
                    "configure",
                )?;
                run_logged(
                    Command::new("make")
                        .current_dir(directory)
                        .env("SOURCE_DATE_EPOCH", &epoch)
                        .arg("-j2"),
                    directory,
                    "make",
                )
            }
            Self::Windows { root } => {
                // Only these child processes use MinGW. Cargo and the application remain MSVC.
                let configure = r#"set -eu
export PATH=/ucrt64/bin:/usr/bin
source_dir=$(/usr/bin/cygpath -u "$LIBRARY_MANAGER_MOBI_SOURCE")
build_dir=$(/usr/bin/cygpath -u "$LIBRARY_MANAGER_MOBI_BUILD")
export CC=/ucrt64/bin/gcc AR=/ucrt64/bin/ar RANLIB=/ucrt64/bin/ranlib
export CFLAGS="-O2 -ffile-prefix-map=\"$source_dir\"=/libmobi -ffile-prefix-map=\"$build_dir\"=/libmobi-build"
export CPPFLAGS=-I/ucrt64/include
export LDFLAGS="-L/ucrt64/lib -static -static-libgcc -Wl,--no-insert-timestamp"
cd "$build_dir"
exec /usr/bin/bash "$source_dir/configure" "$@" --host=x86_64-w64-mingw32 --with-zlib=yes
"#;
                run_logged(
                    windows_shell(root, source, directory)?
                        .env("SOURCE_DATE_EPOCH", &epoch)
                        .args([
                            "--noprofile",
                            "--norc",
                            "-c",
                            configure,
                            "libmobi-configure",
                        ])
                        .args(CONFIGURE_ARGUMENTS),
                    directory,
                    "configure",
                )?;
                let make = r#"set -eu
export PATH=/ucrt64/bin:/usr/bin
build_dir=$(/usr/bin/cygpath -u "$LIBRARY_MANAGER_MOBI_BUILD")
cd "$build_dir"
exec /usr/bin/make -j2
"#;
                run_logged(
                    windows_shell(root, source, directory)?
                        .env("SOURCE_DATE_EPOCH", &epoch)
                        .args(["--noprofile", "--norc", "-c", make]),
                    directory,
                    "make",
                )
            }
        }
    }

    fn validate_engine(&self, engine: &Path, directory: &Path) -> io::Result<()> {
        if let Self::Windows { root } = self {
            let output = Command::new(root.join("ucrt64/bin/objdump.exe"))
                .arg("-p")
                .arg(engine)
                .env("LC_ALL", "C")
                .output()?;
            fs::write(directory.join("dependencies.log"), &output.stdout)?;
            ensure_success(&output, "Windows sidecar dependency inspection")?;
            let imports = String::from_utf8_lossy(&output.stdout);
            if !imports.contains("file format pei-x86-64") {
                return Err(io::Error::other(
                    "The MOBI sidecar must be a native x86_64 Windows PE executable",
                ));
            }
            let mut import_count = 0;
            for line in imports.lines() {
                if let Some(dll) = line.trim().strip_prefix("DLL Name:") {
                    import_count += 1;
                    let dll = dll.trim().to_ascii_lowercase();
                    // UCRT and Windows system APIs are present on supported Windows versions.
                    // Any MinGW, MSYS, zlib or other redistributable DLL must be linked statically.
                    let system =
                        matches!(dll.as_str(), "kernel32.dll" | "msvcrt.dll" | "ucrtbase.dll")
                            || dll.starts_with("api-ms-win-")
                            || dll.starts_with("ext-ms-win-");
                    if !system {
                        return Err(io::Error::other(format!(
                            "The embedded Windows MOBI engine imports a non-system DLL: {dll}; use the static UCRT64 toolchain"
                        )));
                    }
                }
            }
            if import_count == 0 {
                return Err(io::Error::other(
                    "Cannot verify the Windows MOBI sidecar's imported DLLs",
                ));
            }
        }
        Ok(())
    }
}

fn windows_shell(root: &Path, source: &Path, directory: &Path) -> io::Result<Command> {
    let mut command = Command::new(root.join("usr/bin/bash.exe"));
    command
        .current_dir(directory)
        .env("MSYSTEM", "UCRT64")
        .env("LC_ALL", "C")
        .env("LIBRARY_MANAGER_MOBI_SOURCE", windows_shell_path(source)?)
        .env("LIBRARY_MANAGER_MOBI_BUILD", windows_shell_path(directory)?);
    Ok(command)
}

fn windows_shell_path(path: &Path) -> io::Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::other("The native MSYS2 build paths must be valid Unicode"))?;
    // canonicalize() uses a verbatim prefix on Windows; cygpath expects ordinary Win32 paths.
    if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
        Ok(format!(r"\\{path}"))
    } else {
        Ok(path.strip_prefix(r"\\?\").unwrap_or(path).to_owned())
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        io::Error::other("CARGO_MANIFEST_DIR is required to build the embedded MOBI engine")
    })?);
    let target = env::var("TARGET")?;
    let host = env::var("HOST")?;
    let toolchain = Toolchain::native(&target, &host)?;
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
    let engine = build_directory.join(format!("tools/mobitool{}", toolchain.executable_suffix()));
    let stamp = build_directory.join("library-manager-build.sha256");
    let fingerprint = source_fingerprint(&source, &manifest, &target, &toolchain)?;
    if !cache_valid(&stamp, &engine, &fingerprint)? {
        if build_directory.exists() {
            fs::remove_dir_all(&build_directory)?;
        }
        fs::create_dir_all(&build_directory)?;
        toolchain.build(&source, &build_directory)?;
        toolchain.validate_engine(&engine, &build_directory)?;
        let engine_sha256 = hash_file(&engine)?;
        let temporary_stamp = stamp.with_extension("tmp");
        fs::write(
            &temporary_stamp,
            format!("{fingerprint}\n{engine_sha256}\n"),
        )?;
        fs::rename(temporary_stamp, &stamp)?;
    }
    toolchain.validate_engine(&engine, &build_directory)?;

    let binaries = manifest.join("binaries");
    fs::create_dir_all(&binaries)?;
    let sidecar = binaries.join(format!(
        "library-manager-mobitool-{target}{}",
        toolchain.executable_suffix()
    ));
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

fn source_fingerprint(
    source: &Path,
    manifest: &Path,
    target: &str,
    toolchain: &Toolchain,
) -> io::Result<String> {
    let mut files = Vec::new();
    collect_source_files(source, &mut files)?;
    files.sort();
    let mut input = Vec::new();
    for file in files {
        input.extend_from_slice(
            file.strip_prefix(source)
                .map_err(io::Error::other)?
                .as_os_str()
                .as_encoded_bytes(),
        );
        input.push(0);
        input.extend_from_slice(hash_file(&file)?.as_bytes());
        input.push(0);
    }
    input.extend_from_slice(hash_file(&manifest.join("build.rs"))?.as_bytes());
    input.extend_from_slice(LIBMOBI_ARCHIVE_SHA256.as_bytes());
    input.extend_from_slice(target.as_bytes());
    let compiler = toolchain.compiler_version()?;
    input.extend_from_slice(&compiler.stdout);
    if let Toolchain::Windows { root } = toolchain {
        // A changed zlib archive or MSYS tool must not reuse an older successful build.
        for dependency in [
            "usr/bin/bash.exe",
            "usr/bin/make.exe",
            "ucrt64/bin/gcc.exe",
            "ucrt64/bin/ar.exe",
            "ucrt64/bin/ranlib.exe",
            "ucrt64/bin/objdump.exe",
            "ucrt64/include/zlib.h",
            "ucrt64/lib/libz.a",
        ] {
            input.push(0);
            input.extend_from_slice(dependency.as_bytes());
            input.push(0);
            input.extend_from_slice(hash_file(&root.join(dependency))?.as_bytes());
        }
    }
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
    Ok(format!("{:x}", Sha256::digest(&input)))
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

fn hash_file(file: &Path) -> io::Result<String> {
    let mut file = File::open(file)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(format!("{:x}", digest.finalize()))
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
