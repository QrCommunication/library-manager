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
// The release archive already supplies these generated Autotools inputs. Git checkout
// timestamps must not trigger their regeneration with the maintainer's exact tool versions.
const MAINTAINER_GENERATED_INPUTS: &[&str] = &[
    "aclocal.m4",
    "configure",
    "config.h.in",
    "Makefile.in",
    "src/Makefile.in",
    "tools/Makefile.in",
    "tests/Makefile.in",
];
const WINDOWS_MAKE_SCRIPT: &str = r#"set -eu
export PATH=/ucrt64/bin:/usr/bin
source_dir=$(/usr/bin/cygpath -u "$LIBRARY_MANAGER_MOBI_SOURCE")
build_dir=$(/usr/bin/cygpath -u "$LIBRARY_MANAGER_MOBI_BUILD")
make_arguments=(-j2)
maintainer_flags=
for relative in "$@"; do
    generated="$source_dir/$relative"
    make_arguments+=("--old-file=$generated")
    printf -v escaped '%q' "$generated"
    maintainer_flags+=" --old-file=$escaped"
done
cd "$build_dir"
# Libtool -static only freezes Libtool libraries; -all-static reaches the compiler.
# Keep -lz in Libtool's dependency_libs: an absolute .a in library LDFLAGS is
# nested inside libmobi.a instead of propagated to the final executable. The
# explicit UCRT64 search path and -all-static select the real static archive.
exec /usr/bin/make "${make_arguments[@]}" "AM_MAKEFLAGS=$maintainer_flags" \
    "TOOLS_STATIC=-all-static" "LIBZ_LDFLAGS=-L/ucrt64/lib -lz"
"#;
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
                for relative in MAINTAINER_GENERATED_INPUTS {
                    if !source.join(relative).is_file() {
                        return Err(io::Error::other(format!(
                            "The libmobi release is missing generated Autotools input: {relative}"
                        )));
                    }
                }
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
                // --old-file freezes only distributed generated inputs; C sources, objects,
                // config.status and output Makefiles retain their real dependency rules.
                // GNU make does not forward -o to recursive make, so AM_MAKEFLAGS carries it.
                run_logged(
                    windows_shell(root, source, directory)?
                        .env("SOURCE_DATE_EPOCH", &epoch)
                        .args([
                            "--noprofile",
                            "--norc",
                            "-c",
                            WINDOWS_MAKE_SCRIPT,
                            "libmobi-make",
                        ])
                        .args(MAINTAINER_GENERATED_INPUTS),
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
    Ok(Sha256::digest(&input)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // This ELF counterpart proves Libtool semantics; Windows CI audits the real PE imports.
    #[cfg(target_os = "linux")]
    #[test]
    fn windows_link_policy_avoids_external_shared_zlib() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("libmobi-static-{}-{unique}", std::process::id()));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let manifest = env::var_os("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().unwrap().join("src-tauri"));
        let source = manifest.parent().unwrap().join("vendor/libmobi-0.12");
        let output = Command::new(source.join("configure"))
            .args(CONFIGURE_ARGUMENTS)
            .arg("--with-zlib=no")
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Competing libraries expose different results, proving which bytes the linker used.
        fs::write(root.join("dynamic.c"), "int qa_value(void) { return 1; }\n").unwrap();
        fs::write(root.join("archive.c"), "int qa_value(void) { return 2; }\n").unwrap();
        fs::write(
            root.join("main.c"),
            "int qa_value(void); int main(void) { return qa_value() != 2; }\n",
        )
        .unwrap();
        for arguments in [
            vec!["-shared", "-fPIC", "dynamic.c", "-o", "libz.so"],
            vec!["-c", "archive.c", "-o", "archive.o"],
            vec!["-c", "main.c", "-o", "main.o"],
        ] {
            assert!(
                Command::new("cc")
                    .args(arguments)
                    .current_dir(&root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        assert!(
            Command::new("ar")
                .args(["rcs", "libz.a", "archive.o"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let linker = format!(
            "all:\n\t/bin/sh ./libtool --mode=link cc $(TOOLS_STATIC) -L{} main.o $(LIBZ_LDFLAGS) -o fixture\n",
            root.display()
        );
        fs::write(root.join("fixture.mk"), &linker).unwrap();
        let red = Command::new("make")
            .args([
                "-f",
                "fixture.mk",
                "TOOLS_STATIC=-static",
                "LIBZ_LDFLAGS=-lz",
            ])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            red.status.success(),
            "{}",
            String::from_utf8_lossy(&red.stderr)
        );
        assert!(
            !Command::new(root.join("fixture"))
                .env("LD_LIBRARY_PATH", &root)
                .status()
                .unwrap()
                .success()
        );
        // Use the production make script, changing only OS path conversion and the
        // fixture archive location. The effective Libtool/GCC link remains real.
        fs::write(root.join("Makefile"), linker).unwrap();
        let script = WINDOWS_MAKE_SCRIPT
            .replace("/usr/bin/cygpath -u", "/usr/bin/printf %s")
            .replace("/ucrt64/lib", root.to_str().unwrap());
        let green = Command::new("/usr/bin/bash")
            .args(["--noprofile", "--norc", "-c", &script, "libmobi-make"])
            .env("LIBRARY_MANAGER_MOBI_SOURCE", &source)
            .env("LIBRARY_MANAGER_MOBI_BUILD", &root)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            green.status.success(),
            "{}",
            String::from_utf8_lossy(&green.stderr)
        );
        assert!(
            Command::new(root.join("fixture"))
                .env_remove("LD_LIBRARY_PATH")
                .status()
                .unwrap()
                .success()
        );
        // The compiler must really receive -static, rather than Libtool consuming it.
        assert!(
            String::from_utf8_lossy(&green.stdout)
                .lines()
                .any(|line| line.contains("libtool: link:") && line.contains("-static"))
        );
    }

    // The program depends on zlib only through libmobi, matching util.c's
    // uncompress call. A direct main -> zlib fixture cannot detect a nested archive.
    #[cfg(target_os = "linux")]
    #[test]
    fn windows_link_policy_resolves_zlib_through_libmobi() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "libmobi-transitive-static-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let manifest = env::var_os("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().unwrap().join("src-tauri"));
        let source = manifest.parent().unwrap().join("vendor/libmobi-0.12");
        let output = Command::new(source.join("configure"))
            .args(CONFIGURE_ARGUMENTS)
            .arg("--with-zlib=no")
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(output.status.success());
        let archive = Command::new("cc")
            .arg("-print-file-name=libz.a")
            .output()
            .unwrap();
        assert!(archive.status.success());
        let archive = PathBuf::from(String::from_utf8(archive.stdout).unwrap().trim())
            .canonicalize()
            .expect("The link fixture requires the native static zlib development archive");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tools")).unwrap();
        fs::write(root.join("src/library.c"),
            "#include <zlib.h>\nint fixture_mobi(void) { unsigned char out[1]; unsigned long length = 1; const unsigned char compressed[] = {120,156,3,0,0,0,0,1}; return uncompress(out, &length, compressed, sizeof compressed); }\n"
        ).unwrap();
        fs::write(
            root.join("tools/main.c"),
            "int fixture_mobi(void); int main(void) { return fixture_mobi(); }\n",
        )
        .unwrap();
        fs::write(
            root.join("Makefile"),
            "all:\n\t$(MAKE) $(AM_MAKEFLAGS) -C src\n\t$(MAKE) $(AM_MAKEFLAGS) -C tools\n",
        )
        .unwrap();
        fs::write(root.join("src/Makefile"),
            "all: libmobi.la\nlibmobi.lo: library.c\n\t/bin/sh ../libtool --mode=compile cc -c library.c -o libmobi.lo\nlibmobi.la: libmobi.lo\n\t/bin/sh ../libtool --mode=link cc -rpath $(CURDIR)/install $(LIBZ_LDFLAGS) libmobi.lo -o libmobi.la\n"
        ).unwrap();
        fs::write(root.join("tools/Makefile"),
            "all: fixture\nmain.o: main.c\n\tcc -c main.c -o main.o\nfixture: main.o ../src/libmobi.la\n\t/bin/sh ../libtool --mode=link cc $(TOOLS_STATIC) main.o ../src/libmobi.la -o fixture\n"
        ).unwrap();
        // The former absolute LIBZ_LDFLAGS embeds libz.a as an archive member;
        // Libtool drops its dependency record and the final executable cannot link.
        let red = Command::new("make")
            .args(["TOOLS_STATIC=-all-static"])
            .arg(format!("LIBZ_LDFLAGS={}", archive.display()))
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(!red.status.success());
        assert!(
            String::from_utf8_lossy(&red.stderr).contains("undefined reference to `uncompress'")
        );
        let red_metadata = fs::read_to_string(root.join("src/libmobi.la")).unwrap();
        assert!(red_metadata.contains("dependency_libs=''"));
        fs::remove_file(root.join("src/libmobi.la")).unwrap();
        let script = WINDOWS_MAKE_SCRIPT
            .replace("/usr/bin/cygpath -u", "/usr/bin/printf %s")
            .replace("/ucrt64/lib", archive.parent().unwrap().to_str().unwrap());
        let green = Command::new("/usr/bin/bash")
            .args(["--noprofile", "--norc", "-c", &script, "libmobi-make"])
            .env("LIBRARY_MANAGER_MOBI_SOURCE", &source)
            .env("LIBRARY_MANAGER_MOBI_BUILD", &root)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            green.status.success(),
            "{}",
            String::from_utf8_lossy(&green.stderr)
        );
        let metadata = fs::read_to_string(root.join("src/libmobi.la")).unwrap();
        assert!(
            metadata
                .lines()
                .any(|line| line.starts_with("dependency_libs=") && line.contains("-lz"))
        );
        let members = Command::new("ar")
            .args(["t", "src/.libs/libmobi.a"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(members.status.success());
        assert!(
            !String::from_utf8_lossy(&members.stdout)
                .lines()
                .any(|line| line == "libz.a")
        );
        assert!(
            Command::new(root.join("tools/fixture"))
                .status()
                .unwrap()
                .success()
        );
        let dynamic = Command::new("readelf")
            .args(["-d", "tools/fixture"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(dynamic.status.success());
        assert!(!String::from_utf8_lossy(&dynamic.stdout).contains("NEEDED"));
    }

    #[test]
    fn release_generated_inputs_stay_frozen_in_recursive_make() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("libmobi-make-{}-{unique}", std::process::id()));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let source = root.join("release");
        let build = root.join("build");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&build).unwrap();
        for relative in MAINTAINER_GENERATED_INPUTS {
            let path = source.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "release-generated input").unwrap();
        }
        fs::write(source.join("source.c"), "int fixture(void) { return 0; }\n").unwrap();
        let source_name = source.to_str().unwrap();
        fs::write(build.join("Makefile"), format!(
            "all: recurse\nMakefile: {source_name}/aclocal.m4\n{source_name}/aclocal.m4: FORCE\n\t@echo unexpected-maintainer >&2; false\nrecurse:\n\t$(MAKE) $(AM_MAKEFLAGS) -f child.mk all\n.PHONY: all recurse FORCE\n"
        )).unwrap();
        fs::write(build.join("child.mk"), format!(
            "all: object.o\nchild.mk: {source_name}/src/Makefile.in\n{source_name}/src/Makefile.in: FORCE\n\t@echo unexpected-maintainer >&2; false\nobject.o: {source_name}/source.c\n\tcc -c {source_name}/source.c -o object.o\n.PHONY: all FORCE\n"
        )).unwrap();
        let red = Command::new("/usr/bin/make")
            .arg("-j2")
            .current_dir(&build)
            .output()
            .unwrap();
        assert!(!red.status.success());
        assert!(String::from_utf8_lossy(&red.stderr).contains("unexpected-maintainer"));
        // Cygpath is the only Windows-specific operation; identity conversion lets native
        // GNU make exercise the production argument propagation and real C compilation.
        let script = WINDOWS_MAKE_SCRIPT.replace("/usr/bin/cygpath -u", "/usr/bin/printf %s");
        let green = Command::new("/usr/bin/bash")
            .args(["--noprofile", "--norc", "-c", &script, "libmobi-make"])
            .args(MAINTAINER_GENERATED_INPUTS)
            .env("LIBRARY_MANAGER_MOBI_SOURCE", &source)
            .env("LIBRARY_MANAGER_MOBI_BUILD", &build)
            .current_dir(&build)
            .output()
            .unwrap();
        assert!(
            green.status.success(),
            "{}",
            String::from_utf8_lossy(&green.stderr)
        );
        assert!(build.join("object.o").is_file());
        for relative in MAINTAINER_GENERATED_INPUTS {
            assert_eq!(
                fs::read_to_string(source.join(relative)).unwrap(),
                "release-generated input"
            );
        }
        // An ordinary invalid C edit must still fail; only maintainer inputs are frozen.
        fs::write(source.join("source.c"), "invalid C fixture syntax\n").unwrap();
        fs::remove_file(build.join("object.o")).unwrap();
        let invalid = Command::new("/usr/bin/bash")
            .args(["--noprofile", "--norc", "-c", &script, "libmobi-make"])
            .args(MAINTAINER_GENERATED_INPUTS)
            .env("LIBRARY_MANAGER_MOBI_SOURCE", &source)
            .env("LIBRARY_MANAGER_MOBI_BUILD", &build)
            .current_dir(&build)
            .output()
            .unwrap();
        assert!(!invalid.status.success());
        assert!(!String::from_utf8_lossy(&invalid.stderr).contains("unexpected-maintainer"));
    }
}
