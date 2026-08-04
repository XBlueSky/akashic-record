//! Golden tests for the extraction layer. Each fixture's `.golden.json` is the
//! NEW walker's full normalized output, committed + hand-verified. Run
//! `UPDATE_GOLDEN=1 cargo test golden` to (re)generate; plain `cargo test golden`
//! asserts byte-equality against the committed snapshot.

use crate::{EdgeEndpoint, ExtractionOutput, ExtractorKind};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn normalize(o: &ExtractionOutput) -> ExtractionOutput {
    let mut out = o.clone();
    out.chunks
        .sort_by(|a, b| (a.start_byte, &a.name).cmp(&(b.start_byte, &b.name)));
    out.edges.sort_by_key(|e| {
        let tgt = match &e.target {
            EdgeEndpoint::Name { name, .. } => name.clone(),
            EdgeEndpoint::ByteRange { start, .. } => format!("byte:{start}"),
            EdgeEndpoint::Resolved(u) => u.to_string(),
        };
        let kind_str = serde_json::to_string(&e.kind).unwrap_or_else(|_| format!("{:?}", e.kind));
        (kind_str, tgt, e.line.unwrap_or(0))
    });
    out
}

fn run_golden(rel: &str) {
    let dir = fixtures_dir();
    let source_path = dir.join(rel);
    let golden_path = source_path.with_extension(format!(
        "{}.golden.json",
        source_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
    ));
    let source =
        std::fs::read(&source_path).unwrap_or_else(|e| panic!("read {source_path:?}: {e}"));

    let extractor = crate::registry::lookup_for_path(rel);
    let output = match extractor {
        ExtractorKind::TreeSitter(cfg) => crate::extract(cfg, &source, rel).unwrap(),
        ExtractorKind::Custom(sp) => sp
            .parse(std::str::from_utf8(&source).unwrap(), rel)
            .unwrap(),
    };
    let actual = serde_json::to_string_pretty(&normalize(&output)).unwrap();

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&golden_path, format!("{actual}\n")).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&golden_path)
        .unwrap_or_else(|e| panic!("read {golden_path:?}: {e} — run UPDATE_GOLDEN=1 first"));
    assert_eq!(
        actual.trim(),
        expected.trim(),
        "snapshot mismatch for {rel}"
    );
}

#[test]
fn c_01_basic_functions() {
    run_golden("c/01-basic-functions.c");
}
#[test]
fn c_02_structs() {
    run_golden("c/02-structs.c");
}
#[test]
fn c_03_enums() {
    run_golden("c/03-enums.c");
}
#[test]
fn c_04_includes() {
    run_golden("c/04-includes.c");
}
#[test]
fn c_05_macros() {
    run_golden("c/05-macros.c");
}
#[test]
fn c_06_doc() {
    run_golden("c/06-doc.c");
}

#[test]
fn ts_01_basic_exports() {
    run_golden("typescript/01-basic-exports.ts");
}
#[test]
fn ts_02_class_methods() {
    run_golden("typescript/02-class-methods.ts");
}
#[test]
fn ts_03_arrow_fns() {
    run_golden("typescript/03-arrow-fns.ts");
}
#[test]
fn ts_04_re_export() {
    run_golden("typescript/04-re-export.ts");
}
#[test]
fn ts_05_imports() {
    run_golden("typescript/05-imports.ts");
}
#[test]
fn ts_06_nested() {
    run_golden("typescript/06-nested.ts");
}
#[test]
fn ts_07_inheritance() {
    run_golden("typescript/07-inheritance.ts");
}
#[test]
fn ts_08_doc() {
    run_golden("typescript/08-doc.ts");
}
#[test]
fn js_01_functions() {
    run_golden("javascript/01-functions.js");
}
#[test]
fn js_02_class() {
    run_golden("javascript/02-class.js");
}
#[test]
fn js_03_imports() {
    run_golden("javascript/03-imports.mjs");
}
#[test]
fn js_04_jsx() {
    run_golden("javascript/04-jsx.jsx");
}
#[test]
fn js_05_nested() {
    run_golden("javascript/05-nested.js");
}
#[test]
fn js_06_doc() {
    run_golden("javascript/06-doc.js");
}

#[test]
fn cpp_01_classes() {
    run_golden("cpp/01-classes.cpp");
}
#[test]
fn cpp_02_templates() {
    run_golden("cpp/02-templates.cpp");
}
#[test]
fn cpp_03_functions() {
    run_golden("cpp/03-functions.cpp");
}
#[test]
fn cpp_04_namespaces() {
    run_golden("cpp/04-namespaces.cpp");
}
#[test]
fn cpp_05_enums() {
    run_golden("cpp/05-enums.cpp");
}
#[test]
fn cpp_06_inheritance() {
    run_golden("cpp/06-inheritance.cpp");
}

#[test]
fn rust_01_functions() {
    run_golden("rust/01-functions.rs");
}
#[test]
fn rust_02_structs() {
    run_golden("rust/02-structs.rs");
}
#[test]
fn rust_03_impl_block() {
    run_golden("rust/03-impl-block.rs");
}
#[test]
fn rust_04_traits() {
    run_golden("rust/04-traits.rs");
}
#[test]
fn rust_05_enums() {
    run_golden("rust/05-enums.rs");
}
#[test]
fn rust_06_uses() {
    run_golden("rust/06-uses.rs");
}
#[test]
fn rust_07_macros() {
    run_golden("rust/07-macros.rs");
}
#[test]
fn rust_08_nested() {
    run_golden("rust/08-nested.rs");
}
#[test]
fn rust_09_typerefs() {
    run_golden("rust/09-typerefs.rs");
}
#[test]
fn rust_10_impls() {
    run_golden("rust/10-impls.rs");
}
#[test]
fn rust_11_generic_qualified_impls() {
    run_golden("rust/11-generic-qualified-impls.rs");
}

#[test]
fn go_01_functions() {
    run_golden("go/01-functions.go");
}
#[test]
fn go_02_methods() {
    run_golden("go/02-methods.go");
}
#[test]
fn go_03_interfaces() {
    run_golden("go/03-interfaces.go");
}
#[test]
fn go_04_structs() {
    run_golden("go/04-structs.go");
}
#[test]
fn go_05_imports() {
    run_golden("go/05-imports.go");
}

#[test]
fn py_01_functions() {
    run_golden("python/01-functions.py");
}
#[test]
fn py_02_classes() {
    run_golden("python/02-classes.py");
}
#[test]
fn py_03_decorators() {
    run_golden("python/03-decorators.py");
}
#[test]
fn py_04_imports() {
    run_golden("python/04-imports.py");
}
#[test]
fn py_05_typing() {
    run_golden("python/05-typing.py");
}
#[test]
fn py_06_async() {
    run_golden("python/06-async.py");
}

#[test]
fn py_07_nested() {
    run_golden("python/07-nested.py");
}

#[test]
fn cs_01_class() {
    run_golden("csharp/01-class.cs");
}
#[test]
fn cs_02_interface() {
    run_golden("csharp/02-interface.cs");
}
#[test]
fn cs_03_enum() {
    run_golden("csharp/03-enum.cs");
}
#[test]
fn cs_04_imports() {
    run_golden("csharp/04-imports.cs");
}
#[test]
fn cs_05_inheritance() {
    run_golden("csharp/05-inheritance.cs");
}

#[test]
fn java_01_class() {
    run_golden("java/01-class.java");
}
#[test]
fn java_02_interface() {
    run_golden("java/02-interface.java");
}
#[test]
fn java_03_enum() {
    run_golden("java/03-enum.java");
}
#[test]
fn java_04_imports() {
    run_golden("java/04-imports.java");
}
#[test]
fn java_05_inheritance() {
    run_golden("java/05-inheritance.java");
}
#[test]
fn java_06_object_creation() {
    run_golden("java/06-object-creation.java");
}

#[test]
fn rb_01_class() {
    run_golden("ruby/01-class.rb");
}
#[test]
fn rb_02_module() {
    run_golden("ruby/02-module.rb");
}
#[test]
fn rb_03_mixin() {
    run_golden("ruby/03-mixin.rb");
}
#[test]
fn rb_04_require() {
    run_golden("ruby/04-require.rb");
}

#[test]
fn bash_01_functions() {
    run_golden("bash/01-functions.sh");
}
#[test]
fn bash_02_source() {
    run_golden("bash/02-source.sh");
}
#[test]
fn bash_03_nested() {
    run_golden("bash/03-nested.sh");
}
#[test]
fn bash_04_calls() {
    run_golden("bash/04-calls.sh");
}

#[test]
fn lua_01_functions() {
    run_golden("lua/01-functions.lua");
}
#[test]
fn lua_02_nested() {
    run_golden("lua/02-nested.lua");
}
#[test]
fn lua_03_require() {
    run_golden("lua/03-require.lua");
}
#[test]
fn lua_04_methods() {
    run_golden("lua/04-methods.lua");
}

#[test]
fn luau_01_functions() {
    run_golden("luau/01-functions.luau");
}
#[test]
fn luau_02_nested() {
    run_golden("luau/02-nested.luau");
}
#[test]
fn luau_03_require() {
    run_golden("luau/03-require.luau");
}
#[test]
fn luau_04_types() {
    run_golden("luau/04-types.luau");
}

#[test]
fn php_01_class() {
    run_golden("php/01-class.php");
}
#[test]
fn php_02_interface() {
    run_golden("php/02-interface.php");
}
#[test]
fn php_03_enum() {
    run_golden("php/03-enum.php");
}
#[test]
fn php_04_imports() {
    run_golden("php/04-imports.php");
}

#[test]
fn objc_01_class() {
    run_golden("objc/01-class.m");
}
#[test]
fn objc_02_protocol() {
    run_golden("objc/02-protocol.m");
}
#[test]
fn objc_03_cfunc() {
    run_golden("objc/03-cfunc.m");
}
#[test]
fn objc_04_imports() {
    run_golden("objc/04-imports.m");
}

#[test]
fn swift_01_class() {
    run_golden("swift/01-class.swift");
}
#[test]
fn swift_02_struct_protocol() {
    run_golden("swift/02-struct-protocol.swift");
}
#[test]
fn swift_03_enum() {
    run_golden("swift/03-enum.swift");
}
#[test]
fn swift_04_imports() {
    run_golden("swift/04-imports.swift");
}
#[test]
fn swift_05_nested() {
    run_golden("swift/05-nested.swift");
}

#[test]
fn kt_01_class() {
    run_golden("kotlin/01-class.kt");
}
#[test]
fn kt_02_interface_object() {
    run_golden("kotlin/02-interface-object.kt");
}
#[test]
fn kt_03_enum() {
    run_golden("kotlin/03-enum.kt");
}
#[test]
fn kt_04_imports() {
    run_golden("kotlin/04-imports.kt");
}
#[test]
fn kt_05_nested() {
    run_golden("kotlin/05-nested.kt");
}

#[test]
fn scala_01_class() {
    run_golden("scala/01-class.scala");
}
#[test]
fn scala_02_trait_object() {
    run_golden("scala/02-trait-object.scala");
}
#[test]
fn scala_03_enum() {
    run_golden("scala/03-enum.scala");
}
#[test]
fn scala_04_imports() {
    run_golden("scala/04-imports.scala");
}
#[test]
fn scala_05_nested() {
    run_golden("scala/05-nested.scala");
}

#[test]
fn dart_01_class() {
    run_golden("dart/01-class.dart");
}
#[test]
fn dart_02_mixin() {
    run_golden("dart/02-mixin.dart");
}
#[test]
fn dart_03_enum() {
    run_golden("dart/03-enum.dart");
}
#[test]
fn dart_04_imports() {
    run_golden("dart/04-imports.dart");
}
#[test]
fn dart_05_nested() {
    run_golden("dart/05-nested.dart");
}
#[test]
fn dart_06_inheritance() {
    run_golden("dart/06-inheritance.dart");
}
#[test]
fn dart_07_accessors() {
    run_golden("dart/07-accessors.dart");
}

#[test]
fn vue_01_basic() {
    run_golden("vue/01-basic.vue");
}
#[test]
fn vue_02_typescript() {
    run_golden("vue/02-typescript.vue");
}
#[test]
fn vue_03_composition() {
    run_golden("vue/03-composition.vue");
}

#[test]
fn md_01_basic() {
    run_golden("markdown/01-basic.md");
}
#[test]
fn md_02_nested() {
    run_golden("markdown/02-nested.md");
}
#[test]
fn md_03_with_code() {
    run_golden("markdown/03-with-code.md");
}
#[test]
fn md_04_readme() {
    run_golden("markdown/04-readme.md");
}

#[test]
fn dockerfile_01_multistage() {
    run_golden("dockerfile/01-multistage.dockerfile");
}
#[test]
fn dockerfile_02_single() {
    run_golden("dockerfile/02-single.dockerfile");
}
#[test]
fn dockerfile_03_continuation() {
    run_golden("dockerfile/03-continuation.dockerfile");
}
#[test]
fn dockerfile_04_from_stage() {
    run_golden("dockerfile/04-from-stage.dockerfile");
}

#[test]
fn nginx_01_server() {
    run_golden("nginx/01-server.conf");
}
#[test]
fn nginx_02_upstream() {
    run_golden("nginx/02-upstream.conf");
}
#[test]
fn nginx_03_include() {
    run_golden("nginx/03-include.conf");
}
#[test]
fn nginx_04_nested() {
    run_golden("nginx/04-nested.conf");
}

#[test]
fn mustache_01_sections() {
    run_golden("mustache/01-sections.mustache");
}
#[test]
fn mustache_02_partials() {
    run_golden("mustache/02-partials.mustache");
}
#[test]
fn mustache_03_inverted() {
    run_golden("mustache/03-inverted.mustache");
}
#[test]
fn mustache_04_mixed() {
    run_golden("mustache/04-mixed.mustache");
}

#[test]
fn gitlab_ci_01_simple() {
    run_golden("gitlab-ci/01-simple/.gitlab-ci.yml");
}
#[test]
fn gitlab_ci_02_extends() {
    run_golden("gitlab-ci/02-extends/.gitlab-ci.yml");
}
#[test]
fn gitlab_ci_03_include() {
    run_golden("gitlab-ci/03-include/.gitlab-ci.yml");
}
#[test]
fn gitlab_ci_04_objneeds() {
    run_golden("gitlab-ci/04-objneeds/.gitlab-ci.yml");
}

#[test]
fn make_01_rules() {
    run_golden("make/01-rules.mk");
}
#[test]
fn make_02_vars() {
    run_golden("make/02-vars.mk");
}
#[test]
fn make_03_multi_include() {
    run_golden("make/03-multi-include.mk");
}
#[test]
fn make_04_pattern() {
    run_golden("make/04-pattern.mk");
}
#[test]
fn make_05_target_vars() {
    run_golden("make/05-target-vars.mk");
}

#[test]
fn package_json_01_basic() {
    run_golden("package-json/01-basic/package.json");
}
#[test]
fn package_json_02_scripts() {
    run_golden("package-json/02-scripts/package.json");
}
#[test]
fn package_json_03_minimal() {
    run_golden("package-json/03-minimal/package.json");
}
#[test]
fn package_json_04_all_dep_types() {
    run_golden("package-json/04-all-dep-types/package.json");
}

#[test]
fn cargo_toml_01_basic() {
    run_golden("cargo-toml/01-basic/Cargo.toml");
}
#[test]
fn cargo_toml_02_dev_build() {
    run_golden("cargo-toml/02-dev-build/Cargo.toml");
}
#[test]
fn cargo_toml_03_git_path() {
    run_golden("cargo-toml/03-git-path/Cargo.toml");
}
#[test]
fn cargo_toml_04_workspace() {
    run_golden("cargo-toml/04-workspace/Cargo.toml");
}

#[test]
fn pyproject_toml_01_pep621() {
    run_golden("pyproject-toml/01-pep621/pyproject.toml");
}
#[test]
fn pyproject_toml_02_pep621_optional() {
    run_golden("pyproject-toml/02-pep621-optional/pyproject.toml");
}
#[test]
fn pyproject_toml_03_poetry() {
    run_golden("pyproject-toml/03-poetry/pyproject.toml");
}
#[test]
fn pyproject_toml_04_build_system() {
    run_golden("pyproject-toml/04-build-system/pyproject.toml");
}

#[test]
fn hcl_01_resources() {
    run_golden("hcl/01-resources.tf");
}
#[test]
fn hcl_02_module() {
    run_golden("hcl/02-module.tf");
}
#[test]
fn hcl_03_comments_heredoc() {
    run_golden("hcl/03-comments-heredoc.tf");
}
#[test]
fn hcl_04_variables() {
    run_golden("hcl/04-variables.tf");
}
#[test]
fn hcl_05_references() {
    run_golden("hcl/05-references.tf");
}
#[test]
fn cpp_07_template_base() {
    run_golden("cpp/07-template-base.cpp");
}
#[test]
fn cpp_08_doc() {
    run_golden("cpp/08-doc.cpp");
}
#[test]
fn csharp_06_enum_members_and_alias() {
    run_golden("csharp/06-enum-members-and-alias.cs");
}
#[test]
fn dart_08_typedef() {
    run_golden("dart/08-typedef.dart");
}
#[test]
fn rust_12_pub_use_reexport() {
    run_golden("rust/12-pub-use-reexport.rs");
}
#[test]
fn rust_13_chained_routes() {
    run_golden("rust/13-chained-routes.rs");
}
#[test]
fn rust_14_http_calls() {
    run_golden("rust/14-http-calls.rs");
}
#[test]
fn scala_06_supertypes() {
    run_golden("scala/06-supertypes.scala");
}
#[test]
fn hcl_06_unicode_source() {
    run_golden("hcl/06-unicode-source.tf");
}
#[test]
fn nginx_05_utf8() {
    run_golden("nginx/05-utf8.conf");
}
#[test]
fn markdown_05_whitespace_headings() {
    run_golden("markdown/05-whitespace-headings.md");
}
#[test]
fn ruby_05_routes() {
    run_golden("ruby/05-routes.rb");
}
#[test]
fn gitlab_ci_05_nested_keys() {
    run_golden("gitlab-ci/05-nested-keys/.gitlab-ci.yml");
}
#[test]
fn package_json_05_script_key_collision() {
    run_golden("package-json/05-script-key-collision/package.json");
}
#[test]
fn markdown_06_section_byte_spans() {
    run_golden("markdown/06-section-byte-spans.md");
}
