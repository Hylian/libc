#![allow(unused_must_use)]

extern crate gcc;
extern crate syntex_syntax as syntax;

use std::env;
use std::fs::File;
use std::io::BufWriter;
use std::io::prelude::*;
use std::path::{Path, PathBuf};

use syntax::ast;
use syntax::diagnostic::SpanHandler;
use syntax::parse::token::InternedString;
use syntax::attr::{self, ReprAttr};
use syntax::parse::{self, ParseSess};
use syntax::visit::{self, Visitor};

struct TestGenerator<'a> {
    rust: Box<Write>,
    c: Box<Write>,
    sh: &'a SpanHandler,
}

fn main() {
    let target = env::var("TARGET").unwrap();

    let sess = ParseSess::new();
    let src = Path::new("../src/lib.rs");
    let cfg = Vec::new();
    let mut krate = parse::parse_crate_from_file(src, cfg, &sess);
    build_cfg(&mut krate.config, &target);

    let mut gated_cfgs = Vec::new();
    let krate = syntax::config::strip_unconfigured_items(&sess.span_diagnostic,
                                                         krate,
                                                         &mut gated_cfgs);

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let rust_out = BufWriter::new(File::create(out.join("all.rs")).unwrap());
    let mut c_out = BufWriter::new(File::create(out.join("all.c")).unwrap());

    writeln!(c_out, "
#include <glob.h>
#include <ifaddrs.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/ip.h>
#include <pthread.h>
#include <signal.h>
#include <stdalign.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/un.h>
#include <time.h>
#include <utime.h>
#include <wchar.h>
");

    if target.contains("apple-darwin") {
        writeln!(c_out, "
#include <mach/mach_time.h>
");
    }

    visit::walk_crate(&mut TestGenerator {
        rust: Box::new(rust_out),
        c: Box::new(c_out),
        sh: &sess.span_diagnostic,
    }, &krate);

    gcc::Config::new()
                .file(out.join("all.c"))
                .compile("liball.a");
}

fn build_cfg(cfg: &mut ast::CrateConfig, target: &str) {
    let (arch, target_pointer_width) = if target.starts_with("x86_64") {
        ("x86_64", "64")
    } else if target.starts_with("i686") {
        ("x86", "32")
    } else {
        panic!("unknown arch/pointer width: {}", target)
    };
    let (os, family, env) = if target.contains("unknown-linux") {
        ("linux", "unix", "gnu")
    } else if target.contains("apple-darwin") {
        ("macos", "unix", "")
    } else {
        panic!("unknown os/family width: {}", target)
    };

    let mk = attr::mk_name_value_item_str;
    let s = InternedString::new;
    cfg.push(attr::mk_word_item(s(family)));
    cfg.push(mk(s("target_os"), s(os)));
    cfg.push(mk(s("target_family"), s(family)));
    cfg.push(mk(s("target_arch"), s(arch)));
    // skip endianness
    cfg.push(mk(s("target_pointer_width"), s(target_pointer_width)));
    cfg.push(mk(s("target_env"), s(env)));
}

impl<'a> TestGenerator<'a> {
    fn test_type(&mut self, ty: &str) {
        let cty = if ty.starts_with("c_") {
            let rest = ty[2..].replace("long", " long");
            if rest.starts_with("u") {
                format!("unsigned {}", &rest[1..])
            } else if rest.starts_with("s") && rest != "short" {
                format!("signed {}", &rest[1..])
            } else {
                rest
            }
        } else {
            (match ty {
                "sighandler_t" => return,
                ty => ty,
            }).to_string()
        };
        self.test_size_align(ty, &cty);
    }

    fn test_struct(&mut self, ty: &str, _s: &ast::StructDef) {
        let cty = if ty.starts_with("pthread") || ty == "glob_t" {
            ty.to_string()
        } else if ty == "ip6_mreq" {
            "struct ipv6_mreq".to_string()
        } else {
            format!("struct {}", ty)
        };
        self.test_size_align(ty, &cty);
    }

    fn test_size_align(&mut self, rust: &str, c: &str) {
        writeln!(self.c, r#"
            uint64_t __test_size_{ty}() {{ return sizeof({cty}); }}
            uint64_t __test_align_{ty}() {{ return alignof({cty}); }}
        "#, ty = rust, cty = c);
        writeln!(self.rust, r#"
            #[test]
            fn size_align_{ty}() {{
                extern {{
                    fn __test_size_{ty}() -> u64;
                    fn __test_align_{ty}() -> u64;
                }}
                unsafe {{
                    let a = mem::size_of::<{ty}>() as u64;
                    let b = __test_size_{ty}();
                    assert!(a == b, "bad size: rust {{}} != c {{}}", a, b);
                    let a = mem::align_of::<{ty}>() as u64;
                    let b = __test_align_{ty}();
                    assert!(a == b, "bad align: rust {{}} != c {{}}", a, b);
                }}
            }}
        "#, ty = rust);
    }

    fn assert_no_generics(&self, _i: &ast::Item, generics: &ast::Generics) {
         assert!(generics.lifetimes.len() == 0);
         assert!(generics.ty_params.len() == 0);
         assert!(generics.where_clause.predicates.len() == 0);
    }
}

impl<'a, 'v> Visitor<'v> for TestGenerator<'a> {
     fn visit_item(&mut self, i: &'v ast::Item) {
         match i.node {
             ast::ItemTy(_, ref generics) => {
                 self.assert_no_generics(i, generics);
                 self.test_type(&i.ident.to_string());
             }

             ast::ItemStruct(ref s, ref generics) => {
                 self.assert_no_generics(i, generics);
                 let is_c = i.attrs.iter().any(|a| {
                    attr::find_repr_attrs(self.sh, a).iter().any(|a| {
                        *a == ReprAttr::ReprExtern
                    })
                 });
                 if !is_c {
                     panic!("{} is not marked #[repr(C)]", i.ident);
                 }
                 self.test_struct(&i.ident.to_string(), s);
             }

             _ => {}
         }
         visit::walk_item(self, i)
     }
}
