use std::{env, path::Path};

#[cfg(feature = "httpfs")]
mod openssl;

/// Tells whether we're building for Windows. This is more suitable than a plain
/// `cfg!(windows)`, since the latter does not properly handle cross-compilation
///
/// Note that there is no way to know at compile-time which system we'll be
/// targeting, and this test must be made at run-time (of the build script) See
/// https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
#[allow(dead_code)]
fn win_target() -> bool {
    std::env::var("CARGO_CFG_WINDOWS").is_ok()
}

/// Tells whether a given compiler will be used `compiler_name` is compared to
/// the content of `CARGO_CFG_TARGET_ENV` (and is always lowercase)
///
/// See [`win_target`]
#[allow(dead_code)]
fn is_compiler(compiler_name: &str) -> bool {
    std::env::var("CARGO_CFG_TARGET_ENV").map_or(false, |v| v == compiler_name)
}

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("bindgen.rs");
    #[cfg(feature = "bundled")]
    {
        build_bundled::main(&out_dir, &out_path);
    }
    #[cfg(not(feature = "bundled"))]
    {
        build_linked::main(&out_dir, &out_path)
    }
}

#[cfg(feature = "bundled")]
mod build_bundled {
    use std::{
        collections::{HashMap, HashSet},
        fs,
        path::Path,
    };

    use crate::win_target;

    #[derive(Clone, serde::Deserialize)]
    struct Sources {
        cpp_files: HashSet<String>,
        #[serde(default)]
        c_files: HashSet<String>,
        include_dirs: HashSet<String>,
    }

    #[derive(serde::Deserialize)]
    struct Manifest {
        base: Sources,

        #[allow(unused)]
        extensions: HashMap<String, Sources>,
    }

    #[allow(unused)]
    fn add_extension(
        cfg: &mut cc::Build,
        manifest: &Manifest,
        extension: &str,
        cpp_files: &mut HashSet<String>,
        c_files: &mut HashSet<String>,
        include_dirs: &mut HashSet<String>,
    ) {
        cpp_files.extend(manifest.extensions.get(extension).unwrap().cpp_files.clone());
        c_files.extend(manifest.extensions.get(extension).unwrap().c_files.clone());
        include_dirs.extend(manifest.extensions.get(extension).unwrap().include_dirs.clone());
        cfg.define(
            &format!("DUCKDB_EXTENSION_{}_LINKED", extension.to_uppercase()),
            Some("1"),
        );
    }

    fn untar_archive() {
        // Use checked-in vendor sources when present; avoid unpacking duckdb.tar.gz
        // because it would overwrite local compatibility patches.
        if Path::new("duckdb/src/include/duckdb.h").exists() {
            return;
        }
        let path = "duckdb.tar.gz";

        let tar_gz = std::fs::File::open(path).expect("archive file");
        let tar = flate2::read::GzDecoder::new(tar_gz);
        let mut archive = tar::Archive::new(tar);
        archive.unpack(".").expect("archive");
    }

    fn copy_dir_recursive(src: &Path, dst: &Path) {
        if dst.exists() {
            fs::remove_dir_all(dst).expect("remove existing overlay target");
        }
        fs::create_dir_all(dst).expect("create overlay target");
        for entry in fs::read_dir(src).expect("read overlay source") {
            let entry = entry.expect("read overlay entry");
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if entry.file_type().expect("overlay entry type").is_dir() {
                copy_dir_recursive(&src_path, &dst_path);
            } else {
                fs::copy(&src_path, &dst_path).expect("copy overlay file");
            }
        }
    }

    fn collect_source_files(root: &Path, extension: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        for entry in fs::read_dir(root).expect("read source tree") {
            let entry = entry.expect("read source entry");
            let path = entry.path();
            if entry.file_type().expect("source entry type").is_dir() {
                out.extend(collect_source_files(&path, extension));
            } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
                out.insert(path.to_string_lossy().replace('\\', "/"));
            }
        }
        out
    }

    fn apply_sqlite_scanner_overlay(lib_name: &str) {
        let overlay_root = Path::new("sqlite_scanner_overlay");
        println!("cargo:rerun-if-changed={}", overlay_root.display());

        let scanner_src = overlay_root.join("src");
        let scanner_dst = Path::new(lib_name)
            .join("extension")
            .join("sqlite_scanner")
            .join("src");
        copy_dir_recursive(&scanner_src, &scanner_dst);

        let extension_src = overlay_root.join("duckdb_src").join("main").join("extension");
        let extension_dst = Path::new(lib_name).join("src").join("main").join("extension");
        for file_name in ["extension_helper.cpp", "extension_load.cpp"] {
            fs::copy(extension_src.join(file_name), extension_dst.join(file_name))
                .expect("copy duckdb extension loader overlay");
        }

        patch_template_keyword_compat(lib_name);
    }

    fn replace_all_in_file(path: &Path, replacements: &[(&str, &str)]) {
        let mut content = fs::read_to_string(path).expect("read source file for patch");
        let original = content.clone();
        for (from, to) in replacements {
            content = content.replace(from, to);
        }
        if content != original {
            fs::write(path, content).expect("write patched source file");
        }
    }

    fn patch_template_keyword_compat(lib_name: &str) {
        replace_all_in_file(
            &Path::new(lib_name)
                .join("src")
                .join("execution")
                .join("window_executor.cpp"),
            &[("OP::template Operation", "OP::Operation")],
        );
        replace_all_in_file(
            &Path::new(lib_name)
                .join("src")
                .join("core_functions")
                .join("aggregate")
                .join("distributive")
                .join("arg_min_max.cpp"),
            &[
                ("STATE::template ReadValue", "STATE::ReadValue"),
                (
                    "STATE::template AssignValue(target.value, source.value)",
                    "STATE::AssignValue(target.value, source.value)",
                ),
            ],
        );
        replace_all_in_file(
            &Path::new(lib_name)
                .join("src")
                .join("core_functions")
                .join("aggregate")
                .join("distributive")
                .join("bitagg.cpp"),
            &[
                ("OP::template Assign", "OP::Assign"),
                ("OP::template Execute", "OP::Execute"),
            ],
        );
        replace_all_in_file(
            &Path::new(lib_name)
                .join("src")
                .join("core_functions")
                .join("aggregate")
                .join("distributive")
                .join("minmax.cpp"),
            &[
                (
                    "OP::template Execute(state, input, i, count)",
                    "OP::Execute(state, input, i, count)",
                ),
                (
                    "OP::template Execute(target, *source.value, 0, 1)",
                    "OP::Execute(target, *source.value, 0, 1)",
                ),
            ],
        );
    }

    fn sqlite_scanner_sources(lib_name: &str) -> Sources {
        let src_root = Path::new(lib_name)
            .join("extension")
            .join("sqlite_scanner")
            .join("src");
        let mut cpp_files = collect_source_files(&src_root, "cpp");
        let mut c_files = collect_source_files(&src_root, "c");
        c_files.retain(|path| path.ends_with("/sqlite3.c"));
        cpp_files.retain(|path| !path.ends_with("/sqlite3.c"));

        Sources {
            cpp_files,
            c_files,
            include_dirs: [
                "extension/sqlite_scanner/src/include".to_string(),
                "extension/sqlite_scanner/src/sqlite".to_string(),
                "extension/sqlite_scanner/src".to_string(),
            ]
            .into_iter()
            .collect(),
        }
    }

    pub fn main(out_dir: &str, out_path: &Path) {
        let lib_name = super::lib_name();

        untar_archive();
        apply_sqlite_scanner_overlay(lib_name);

        if !cfg!(feature = "bundled") {
            // This is just a sanity check, the top level `main` should ensure this.
            panic!("This module should not be used: bundled feature has not been enabled");
        }

        #[cfg(feature = "buildtime_bindgen")]
        {
            use super::{bindings, HeaderLocation};
            let header = HeaderLocation::FromPath(format!("{}/src/include/duckdb.h", lib_name));
            bindings::write_to_out_dir(header, out_path);
        }
        #[cfg(not(feature = "buildtime_bindgen"))]
        {
            use std::fs;
            fs::copy("src/bindgen_bundled_version.rs", out_path).expect("Could not copy bindings to output directory");
        }

        let manifest_file = std::fs::File::open(format!("{}/manifest.json", lib_name)).expect("manifest file");
        let mut manifest: Manifest = serde_json::from_reader(manifest_file).expect("reading manifest file");
        manifest
            .extensions
            .insert("sqlite_scanner".to_string(), sqlite_scanner_sources(lib_name));

        let mut cpp_files = HashSet::new();
        let mut c_files = HashSet::new();
        let mut include_dirs = HashSet::new();

        cpp_files.extend(manifest.base.cpp_files.clone());
        c_files.extend(manifest.base.c_files.clone());
        // otherwise clippy will remove the clone here...
        // https://github.com/rust-lang/rust-clippy/issues/9011
        #[allow(clippy::all)]
        include_dirs.extend(manifest.base.include_dirs.clone());

        let mut cfg = cc::Build::new();

        #[cfg(feature = "httpfs")]
        {
            if let Ok((_, openssl_include_dir)) = super::openssl::get_openssl_v2() {
                cfg.include(openssl_include_dir);
            }
            add_extension(
                &mut cfg,
                &manifest,
                "httpfs",
                &mut cpp_files,
                &mut c_files,
                &mut include_dirs,
            );
        }

        #[cfg(feature = "parquet")]
        add_extension(
            &mut cfg,
            &manifest,
            "parquet",
            &mut cpp_files,
            &mut c_files,
            &mut include_dirs,
        );

        #[cfg(feature = "json")]
        add_extension(
            &mut cfg,
            &manifest,
            "json",
            &mut cpp_files,
            &mut c_files,
            &mut include_dirs,
        );

        #[cfg(feature = "sqlite_scanner")]
        add_extension(
            &mut cfg,
            &manifest,
            "sqlite_scanner",
            &mut cpp_files,
            &mut c_files,
            &mut include_dirs,
        );

        // duckdb/tools/pythonpkg/setup.py
        cfg.define("DUCKDB_EXTENSION_AUTOINSTALL_DEFAULT", "0");
        cfg.define("DUCKDB_EXTENSION_AUTOLOAD_DEFAULT", "1");

        // Since the manifest controls the set of files, we require it to be changed to know whether
        // to rebuild the project
        println!("cargo:rerun-if-changed={}/manifest.json", lib_name);
        // Make sure to rebuild the project if tar file changed
        println!("cargo:rerun-if-changed=duckdb.tar.gz");

        cfg.include(lib_name);

        cfg.includes(include_dirs.iter().map(|x| format!("{}/{}", lib_name, x)));

        for f in cpp_files {
            cfg.file(f);
        }

        cfg.cpp(true)
            .flag_if_supported("-std=c++11")
            .flag_if_supported("-stdlib=libc++")
            .flag_if_supported("-stdlib=libstdc++")
            .flag_if_supported("/bigobj")
            .warnings(false)
            .flag_if_supported("-w");

        if win_target() {
            cfg.define("DUCKDB_BUILD_LIBRARY", None);
        }

        cfg.compile(lib_name);
        if !c_files.is_empty() {
            let mut c_cfg = cc::Build::new();

            c_cfg.include(lib_name);
            c_cfg.includes(include_dirs.iter().map(|x| format!("{}/{}", lib_name, x)));

            for f in c_files {
                c_cfg.file(f);
            }

            c_cfg
                .define("SQLITE_ENABLE_FTS5", None)
                .define("SQLITE_ENABLE_FTS4", None)
                .define("SQLITE_ENABLE_FTS3_PARENTHESIS", None)
                .define("SQLITE_ENABLE_RTREE", None)
                .warnings(false)
                .flag_if_supported("-w");

            c_cfg.compile("duckdb_ext_c");
        }
        println!("cargo:lib_dir={out_dir}");
    }
}

fn env_prefix() -> &'static str {
    "DUCKDB"
}

fn lib_name() -> &'static str {
    "duckdb"
}

pub enum HeaderLocation {
    FromEnvironment,
    Wrapper,
    FromPath(String),
}

impl From<HeaderLocation> for String {
    fn from(header: HeaderLocation) -> String {
        match header {
            HeaderLocation::FromEnvironment => {
                let prefix = env_prefix();
                let mut header = env::var(format!("{prefix}_INCLUDE_DIR"))
                    .unwrap_or_else(|_| env::var(format!("{}_LIB_DIR", env_prefix())).unwrap());
                header.push_str("/duckdb.h");
                header
            }
            HeaderLocation::Wrapper => "wrapper.h".into(),
            HeaderLocation::FromPath(path) => path,
        }
    }
}

#[cfg(not(feature = "bundled"))]
mod build_linked {
    #[cfg(feature = "vcpkg")]
    extern crate vcpkg;

    #[cfg(feature = "buildtime_bindgen")]
    use super::bindings;

    use super::{env_prefix, is_compiler, lib_name, win_target, HeaderLocation};
    use std::{env, path::Path};

    pub fn main(_out_dir: &str, out_path: &Path) {
        // We need this to config the LD_LIBRARY_PATH
        #[allow(unused_variables)]
        let header = find_duckdb();

        if !cfg!(feature = "buildtime_bindgen") {
            std::fs::copy("src/bindgen_bundled_version.rs", out_path)
                .expect("Could not copy bindings to output directory");
        } else {
            #[cfg(feature = "buildtime_bindgen")]
            {
                bindings::write_to_out_dir(header, out_path);
            }
        }
    }

    fn find_link_mode() -> &'static str {
        // If the user specifies DUCKDB_STATIC, do static
        // linking, unless it's explicitly set to 0.
        match &env::var(format!("{}_STATIC", env_prefix())) {
            Ok(v) if v != "0" => "static",
            _ => "dylib",
        }
    }
    // Prints the necessary cargo link commands and returns the path to the header.
    fn find_duckdb() -> HeaderLocation {
        let link_lib = lib_name();

        println!("cargo:rerun-if-env-changed={}_INCLUDE_DIR", env_prefix());
        println!("cargo:rerun-if-env-changed={}_LIB_DIR", env_prefix());
        println!("cargo:rerun-if-env-changed={}_STATIC", env_prefix());
        if cfg!(feature = "vcpkg") && is_compiler("msvc") {
            println!("cargo:rerun-if-env-changed=VCPKGRS_DYNAMIC");
        }

        // dependents can access `DEP_DUCKDB_LINK_TARGET` (`duckdb` being the
        // `links=` value in our Cargo.toml) to get this value. This might be
        // useful if you need to ensure whatever crypto library sqlcipher relies
        // on is available, for example.
        println!("cargo:link-target={link_lib}");

        if win_target() && cfg!(feature = "winduckdb") {
            println!("cargo:rustc-link-lib=dylib={link_lib}");
            return HeaderLocation::Wrapper;
        }

        // Allow users to specify where to find DuckDB.
        if let Ok(dir) = env::var(format!("{}_LIB_DIR", env_prefix())) {
            println!("cargo:rustc-env=LD_LIBRARY_PATH={dir}");
            // Try to use pkg-config to determine link commands
            let pkgconfig_path = Path::new(&dir).join("pkgconfig");
            env::set_var("PKG_CONFIG_PATH", pkgconfig_path);
            if pkg_config::Config::new().probe(link_lib).is_err() {
                // Otherwise just emit the bare minimum link commands.
                println!("cargo:rustc-link-lib={}={}", find_link_mode(), link_lib);
                println!("cargo:rustc-link-search={dir}");
            }
            return HeaderLocation::FromEnvironment;
        }

        if let Some(header) = try_vcpkg() {
            return header;
        }

        // See if pkg-config can do everything for us.
        match pkg_config::Config::new().print_system_libs(false).probe(link_lib) {
            Ok(mut lib) => {
                if let Some(mut header) = lib.include_paths.pop() {
                    header.push("duckdb.h");
                    HeaderLocation::FromPath(header.to_string_lossy().into())
                } else {
                    HeaderLocation::Wrapper
                }
            }
            Err(_) => {
                // No env var set and pkg-config couldn't help; just output the link-lib
                // request and hope that the library exists on the system paths. We used to
                // output /usr/lib explicitly, but that can introduce other linking problems;
                // see https://github.com/rusqlite/rusqlite/issues/207.
                println!("cargo:rustc-link-lib={}={}", find_link_mode(), link_lib);
                HeaderLocation::Wrapper
            }
        }
    }

    fn try_vcpkg() -> Option<HeaderLocation> {
        if cfg!(feature = "vcpkg") && is_compiler("msvc") {
            // See if vcpkg can find it.
            if let Ok(mut lib) = vcpkg::Config::new().probe(lib_name()) {
                if let Some(mut header) = lib.include_paths.pop() {
                    header.push("duckdb.h");
                    return Some(HeaderLocation::FromPath(header.to_string_lossy().into()));
                }
            }
            None
        } else {
            None
        }
    }
}

#[cfg(feature = "buildtime_bindgen")]
mod bindings {
    use super::HeaderLocation;

    use std::{fs::OpenOptions, io::Write, path::Path};

    pub fn write_to_out_dir(header: HeaderLocation, out_path: &Path) {
        let header: String = header.into();
        let mut output = Vec::new();
        bindgen::builder()
            .trust_clang_mangling(false)
            .header(header.clone())
            .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
            .generate()
            .unwrap_or_else(|_| panic!("could not run bindgen on header {header}"))
            .write(Box::new(&mut output))
            .expect("could not write output of bindgen");
        let output = String::from_utf8(output).expect("bindgen output was not UTF-8?!");
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(out_path)
            .unwrap_or_else(|_| panic!("Could not write to {out_path:?}"));

        file.write_all(output.as_bytes())
            .unwrap_or_else(|_| panic!("Could not write to {out_path:?}"));
    }
}
