use crate::types::{ExtractorKind, LanguageConfig};
use crate::{languages, special};

static BASH_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::BASH);
static C_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::C);
static CPP_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::CPP);
static CSHARP_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::CSHARP);
static DART_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::DART);
static GO_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::GO);
static JAVA_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::JAVA);
static JAVASCRIPT_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::JAVASCRIPT);
static KOTLIN_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::KOTLIN);
static LUA_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::LUA);
static LUAU_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::LUAU);
static OBJC_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::OBJC);
static PHP_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::PHP);
static PYTHON_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::PYTHON);
static RUBY_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::RUBY);
static RUST_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::RUST);
static SCALA_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::SCALA);
static SWIFT_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::SWIFT);
static TS_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::TYPESCRIPT);
static TSX_EXTRACTOR: ExtractorKind = ExtractorKind::TreeSitter(&languages::TSX);
static VUE_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::VUE);
static MARKDOWN_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::MARKDOWN);
static DOCKERFILE_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::DOCKERFILE);
static NGINX_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::NGINX);
static MUSTACHE_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::MUSTACHE);
static GITLAB_CI_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::GITLAB_CI);
static MAKEFILE_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::MAKEFILE);
static HCL_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::HCL);
static PACKAGE_JSON_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::PACKAGE_JSON);
static CARGO_TOML_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::CARGO_TOML);
static PYPROJECT_TOML_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::PYPROJECT_TOML);
static DRUPAL_ROUTING_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::DRUPAL_ROUTING);
static FALLBACK_EXTRACTOR: ExtractorKind = ExtractorKind::Custom(&special::FALLBACK);

/// All registered extractors. FALLBACK MUST be last (it's the catch-all and its
/// `extensions()` is empty, so it never matches by extension — only as the final
/// fallthrough in `lookup_for_path`).
static EXTRACTORS: &[&ExtractorKind] = &[
    &TSX_EXTRACTOR,
    &TS_EXTRACTOR,
    &JAVASCRIPT_EXTRACTOR,
    &BASH_EXTRACTOR,
    &C_EXTRACTOR,
    &CPP_EXTRACTOR,
    &CSHARP_EXTRACTOR,
    &DART_EXTRACTOR,
    &RUST_EXTRACTOR,
    &SCALA_EXTRACTOR,
    &SWIFT_EXTRACTOR,
    &GO_EXTRACTOR,
    &JAVA_EXTRACTOR,
    &KOTLIN_EXTRACTOR,
    &LUA_EXTRACTOR,
    &LUAU_EXTRACTOR,
    &OBJC_EXTRACTOR,
    &PHP_EXTRACTOR,
    &PYTHON_EXTRACTOR,
    &RUBY_EXTRACTOR,
    &VUE_EXTRACTOR,
    &MARKDOWN_EXTRACTOR,
    &DOCKERFILE_EXTRACTOR,
    &NGINX_EXTRACTOR,
    &MUSTACHE_EXTRACTOR,
    &GITLAB_CI_EXTRACTOR,
    &MAKEFILE_EXTRACTOR,
    &HCL_EXTRACTOR,
    &PACKAGE_JSON_EXTRACTOR,
    &CARGO_TOML_EXTRACTOR,
    &PYPROJECT_TOML_EXTRACTOR,
    &DRUPAL_ROUTING_EXTRACTOR,
    &FALLBACK_EXTRACTOR,
];

fn extractor_extensions(k: &ExtractorKind) -> &'static [&'static str] {
    match k {
        ExtractorKind::TreeSitter(c) => c.extensions,
        ExtractorKind::Custom(sp) => sp.extensions(),
    }
}

fn extractor_filenames(k: &ExtractorKind) -> &'static [&'static str] {
    match k {
        ExtractorKind::TreeSitter(_) => &[],
        ExtractorKind::Custom(sp) => sp.filename_matches(),
    }
}

fn extractor_suffixes(k: &ExtractorKind) -> &'static [&'static str] {
    match k {
        ExtractorKind::TreeSitter(_) => &[],
        ExtractorKind::Custom(sp) => sp.path_suffix_matches(),
    }
}

/// Resolve a file path to its extractor: (1) exact filename match (for
/// filename-tagged Custom extractors), (2) extension match, (3) compound-suffix
/// match (for compound extensions like `.routing.yml`), (4) C/C++ header
/// special-case, (5) FALLBACK.
pub fn lookup_for_path(rel_path: &str) -> &'static ExtractorKind {
    let lower = rel_path.to_lowercase();
    let basename = lower.rsplit('/').next().unwrap_or(&lower);

    // (1) filename exact match
    for &k in EXTRACTORS {
        if extractor_filenames(k).contains(&basename) {
            return k;
        }
    }
    // (2) extension match
    for &k in EXTRACTORS {
        if extractor_extensions(k).iter().any(|e| lower.ends_with(e)) {
            return k;
        }
    }
    // (3) basename suffix match — for compound extensions like `.routing.yml`
    // that are more specific than the final dot-extension alone. Checked after
    // extension dispatch so a plain `.yml` won't be claimed here; a `.routing.yml`
    // suffix is long enough to be unambiguous.
    for &k in EXTRACTORS {
        if extractor_suffixes(k).iter().any(|s| basename.ends_with(*s)) {
            return k;
        }
    }
    // (4) special-case: C/C++ header → C++ (matches old analyzer .h→Cpp).
    // NOTE: `.h` is ambiguous (C / C++ / Objective-C). The ObjC extractor
    // deliberately claims only `.m` / `.mm`; ObjC headers therefore route to
    // C++ here (an accepted known limitation — C++ parses ObjC `@interface`
    // headers as errors but the ObjC `.m` implementation still chunks fully).
    if lower.ends_with(".h") {
        return &CPP_EXTRACTOR;
    }
    // (5) fallback
    &FALLBACK_EXTRACTOR
}

/// Iterate the LanguageConfig of every tree-sitter extractor (for startup
/// validation in main.rs).
pub fn tree_sitter_languages() -> impl Iterator<Item = &'static LanguageConfig> {
    EXTRACTORS.iter().filter_map(|k| match k {
        ExtractorKind::TreeSitter(c) => Some(*c),
        ExtractorKind::Custom(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(k: &ExtractorKind) -> &'static str {
        match k {
            ExtractorKind::TreeSitter(c) => c.name,
            ExtractorKind::Custom(sp) => sp.name(),
        }
    }

    #[test]
    fn resolves_known_extensions() {
        assert_eq!(name_of(lookup_for_path("a/b.ts")), "typescript");
        // .tsx routes to the dedicated TSX config (LANGUAGE_TSX, enables JSX).
        assert_eq!(name_of(lookup_for_path("a/b.tsx")), "tsx");
        // `.js`/`.jsx`/`.mjs` now route to the dedicated JavaScript config
        // (split off the TS grammar in EXT-2).
        assert_eq!(name_of(lookup_for_path("a/b.js")), "javascript");
        assert_eq!(name_of(lookup_for_path("a/b.jsx")), "javascript");
        assert_eq!(name_of(lookup_for_path("a/b.mjs")), "javascript");
        assert_eq!(name_of(lookup_for_path("x.rs")), "rust");
        assert_eq!(name_of(lookup_for_path("m.go")), "go");
        assert_eq!(name_of(lookup_for_path("s.py")), "python");
        assert_eq!(name_of(lookup_for_path("app.rb")), "ruby");
        assert_eq!(name_of(lookup_for_path("setup.sh")), "bash");
        assert_eq!(name_of(lookup_for_path("lib/utils.bash")), "bash");
        assert_eq!(name_of(lookup_for_path("mod.lua")), "lua");
        assert_eq!(name_of(lookup_for_path("mod.luau")), "luau");
        assert_eq!(name_of(lookup_for_path("index.php")), "php");
        assert_eq!(name_of(lookup_for_path("c.c")), "c");
        assert_eq!(name_of(lookup_for_path("p.cpp")), "cpp");
        assert_eq!(name_of(lookup_for_path("v.vue")), "vue");
        assert_eq!(name_of(lookup_for_path("R.md")), "markdown");
        // Dockerfile SpecialExtractor: exact filename + `.dockerfile` extension.
        assert_eq!(name_of(lookup_for_path("Dockerfile")), "dockerfile");
        assert_eq!(name_of(lookup_for_path("ops/Dockerfile")), "dockerfile");
        assert_eq!(name_of(lookup_for_path("Containerfile")), "dockerfile");
        assert_eq!(name_of(lookup_for_path("app.dockerfile")), "dockerfile");
        // Known limitation: `Dockerfile.prod` (suffix on the stem) does NOT
        // match — only exact `Dockerfile` / `.dockerfile` extension.
        assert_eq!(name_of(lookup_for_path("Dockerfile.prod")), "fallback");
        // nginx SpecialExtractor: exact `nginx.conf` filename + broad `.conf`
        // extension (akashic treats all `.conf` as nginx-style — scope choice).
        assert_eq!(name_of(lookup_for_path("nginx.conf")), "nginx");
        assert_eq!(name_of(lookup_for_path("etc/nginx/nginx.conf")), "nginx");
        assert_eq!(name_of(lookup_for_path("conf.d/site.conf")), "nginx");
        // Mustache/Handlebars SpecialExtractor: `.mustache` / `.hbs` /
        // `.handlebars` extensions (no filename match).
        assert_eq!(name_of(lookup_for_path("page.mustache")), "mustache");
        assert_eq!(name_of(lookup_for_path("tpl/header.hbs")), "mustache");
        assert_eq!(name_of(lookup_for_path("x.handlebars")), "mustache");
        // GitLab CI SpecialExtractor: matched ONLY by the canonical filenames
        // `.gitlab-ci.yml` / `.gitlab-ci.yaml`. No `.yml`/`.yaml` extension is
        // claimed, so arbitrary YAML files are NOT hijacked.
        assert_eq!(name_of(lookup_for_path(".gitlab-ci.yml")), "gitlab-ci");
        assert_eq!(name_of(lookup_for_path(".gitlab-ci.yaml")), "gitlab-ci");
        assert_eq!(name_of(lookup_for_path("repo/.gitlab-ci.yml")), "gitlab-ci");
        // A renamed CI file or any other YAML routes to fallback (no hijack).
        assert_eq!(name_of(lookup_for_path("ci/pipeline.yml")), "fallback");
        assert_eq!(name_of(lookup_for_path("config.yaml")), "fallback");
        // package.json SpecialExtractor: matched ONLY by the exact filename
        // `package.json`. No `.json` extension is claimed, so arbitrary JSON
        // files are NOT hijacked (generic `.json` -> fallback).
        assert_eq!(name_of(lookup_for_path("package.json")), "package-json");
        assert_eq!(
            name_of(lookup_for_path("frontend/package.json")),
            "package-json"
        );
        assert_eq!(name_of(lookup_for_path("tsconfig.json")), "fallback");
        assert_eq!(name_of(lookup_for_path("data/config.json")), "fallback");
        // Cargo.toml SpecialExtractor: matched ONLY by the exact filename
        // `Cargo.toml` (case-folded). No `.toml` extension is claimed, so
        // arbitrary TOML files are NOT hijacked (generic `.toml` -> fallback).
        assert_eq!(name_of(lookup_for_path("Cargo.toml")), "cargo-toml");
        assert_eq!(
            name_of(lookup_for_path("crates/core/Cargo.toml")),
            "cargo-toml"
        );
        assert_eq!(name_of(lookup_for_path("rustfmt.toml")), "fallback");
        assert_eq!(name_of(lookup_for_path("config/app.toml")), "fallback");
        // pyproject.toml SpecialExtractor: matched ONLY by the exact filename
        // `pyproject.toml` (case-folded). No `.toml` extension is claimed.
        assert_eq!(name_of(lookup_for_path("pyproject.toml")), "pyproject-toml");
        assert_eq!(
            name_of(lookup_for_path("services/api/pyproject.toml")),
            "pyproject-toml"
        );
        // Makefile SpecialExtractor: exact filenames `Makefile` / `GNUmakefile`
        // (case-folded) + `.mk` / `.make` extensions.
        assert_eq!(name_of(lookup_for_path("Makefile")), "makefile");
        assert_eq!(name_of(lookup_for_path("src/Makefile")), "makefile");
        assert_eq!(name_of(lookup_for_path("makefile")), "makefile");
        assert_eq!(name_of(lookup_for_path("GNUmakefile")), "makefile");
        assert_eq!(name_of(lookup_for_path("rules.mk")), "makefile");
        assert_eq!(name_of(lookup_for_path("build/config.make")), "makefile");
        // Known limitation: a suffixed name like `Makefile.local` (suffix on
        // the stem) does NOT match — only exact name / `.mk` / `.make`.
        assert_eq!(name_of(lookup_for_path("Makefile.local")), "fallback");
        // HCL/Terraform SpecialExtractor: `.tf` + `.hcl` extensions (no
        // filename match). `.tfvars` is value-only and deliberately NOT claimed.
        assert_eq!(name_of(lookup_for_path("main.tf")), "hcl");
        assert_eq!(name_of(lookup_for_path("infra/modules/vpc/main.tf")), "hcl");
        assert_eq!(name_of(lookup_for_path("config.hcl")), "hcl");
        assert_eq!(name_of(lookup_for_path("terraform.tfvars")), "fallback");
        assert_eq!(name_of(lookup_for_path("Program.cs")), "csharp");
        assert_eq!(name_of(lookup_for_path("main.dart")), "dart");
        assert_eq!(name_of(lookup_for_path("App.swift")), "swift");
        assert_eq!(name_of(lookup_for_path("Main.kt")), "kotlin");
        assert_eq!(name_of(lookup_for_path("build.gradle.kts")), "kotlin");
        assert_eq!(name_of(lookup_for_path("Main.scala")), "scala");
        assert_eq!(name_of(lookup_for_path("script.sc")), "scala");
        assert_eq!(name_of(lookup_for_path("Counter.m")), "objc");
        assert_eq!(name_of(lookup_for_path("Counter.mm")), "objc");
        // `.h` stays routed to C++ (ObjC claims only .m/.mm).
        assert_eq!(name_of(lookup_for_path("Counter.h")), "cpp");
    }

    #[test]
    fn header_maps_to_cpp() {
        assert_eq!(name_of(lookup_for_path("foo.h")), "cpp");
        assert_eq!(name_of(lookup_for_path("foo.hpp")), "cpp");
    }

    #[test]
    fn unknown_falls_back() {
        assert_eq!(name_of(lookup_for_path("data.xyz")), "fallback");
        assert_eq!(name_of(lookup_for_path("noext")), "fallback");
        assert_eq!(name_of(lookup_for_path("archive.tar.gz")), "fallback");
    }

    #[test]
    fn tree_sitter_languages_yields_ten() {
        // Count increases by 1 each time a new LanguageConfig is registered.
        // EXT-7-3 Batch B added TSX (separate from TYPESCRIPT).
        assert_eq!(tree_sitter_languages().count(), 20);
    }

    // ── Drupal *.routing.yml suffix dispatch (EXT-7-5 T3) ───────────────────

    #[test]
    fn drupal_routing_yml_dispatch() {
        // Any `<module>.routing.yml` must route to the drupal_routing extractor.
        assert_eq!(
            name_of(lookup_for_path("mymodule.routing.yml")),
            "drupal_routing"
        );
        assert_eq!(
            name_of(lookup_for_path("modules/custom/foo/foo.routing.yml")),
            "drupal_routing"
        );
        // Case-folded path also matches (lookup lowercases the path).
        assert_eq!(
            name_of(lookup_for_path("MyModule.routing.yml")),
            "drupal_routing"
        );
    }

    #[test]
    fn plain_yml_not_hijacked_by_drupal_routing() {
        // A plain `.yml` file with no `.routing` suffix must NOT go to drupal_routing.
        assert_ne!(
            name_of(lookup_for_path("regular.yml")),
            "drupal_routing",
            "plain .yml must not route to drupal_routing"
        );
        assert_ne!(
            name_of(lookup_for_path("ci/pipeline.yml")),
            "drupal_routing"
        );
        // `.gitlab-ci.yml` must continue to route to gitlab-ci (filename match
        // comes before suffix dispatch in lookup_for_path).
        assert_eq!(
            name_of(lookup_for_path(".gitlab-ci.yml")),
            "gitlab-ci",
            ".gitlab-ci.yml must still route to gitlab-ci"
        );
    }
}
