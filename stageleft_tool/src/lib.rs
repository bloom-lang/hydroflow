use std::path::Path;
use std::{env, fs};

use proc_macro2::Span;
use quote::ToTokens;
use syn::parse_quote;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;

struct GenMacroVistor {
    exported_macros: Vec<(String, syn::Path)>,
    current_mod: syn::Path,
}

// marks everything as pub(crate) because proc-macros cannot actually export anything
impl<'a> Visit<'a> for GenMacroVistor {
    fn visit_item_mod(&mut self, i: &'a syn::ItemMod) {
        let old_mod = self.current_mod.clone();
        let i_ident = &i.ident;
        self.current_mod = parse_quote!(#old_mod::#i_ident);

        syn::visit::visit_item_mod(self, i);

        self.current_mod = old_mod;
    }

    fn visit_item_fn(&mut self, i: &'a syn::ItemFn) {
        let is_entry = i
            .attrs
            .iter()
            .any(|a| a.path().to_token_stream().to_string() == "stageleft :: entry");

        if is_entry {
            let cur_path = &self.current_mod;
            let contents = i
                .block
                .to_token_stream()
                .to_string()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>();
            let contents_hash = sha256::digest(contents);
            self.exported_macros
                .push((contents_hash, parse_quote!(#cur_path)));
        }
    }
}

pub fn gen_macro(staged_path: &Path, crate_name: &str) {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("lib.rs");

    let flow_lib =
        syn_inline_mod::parse_and_inline_modules(&staged_path.join("src").join("lib.rs"));
    let mut visitor = GenMacroVistor {
        exported_macros: vec![],
        current_mod: parse_quote!(crate::__staged),
    };
    visitor.visit_file(&flow_lib);

    let staged_path_absolute = fs::canonicalize(staged_path).unwrap();

    let lib_path = staged_path_absolute.join("src").join("lib.rs");
    let lib_path_string = lib_path.to_string_lossy();
    let mut out_file: syn::File = parse_quote!(
        #[path = #lib_path_string] mod __staged;
    );

    for (hash, exported_from) in visitor.exported_macros {
        let underscored_path = syn::Ident::new(&("macro_".to_string() + &hash), Span::call_site());

        let proc_macro_wrapper: syn::ItemFn = parse_quote!(
            #[proc_macro]
            #[allow(non_snake_case)]
            pub fn #underscored_path(input: ::proc_macro::TokenStream) -> ::proc_macro::TokenStream {
                let input = ::stageleft::internal::TokenStream::from(input);
                let out = #exported_from::#underscored_path(input);
                ::proc_macro::TokenStream::from(out)
            }
        );

        out_file.items.push(syn::Item::Fn(proc_macro_wrapper));
    }

    fs::write(dest_path, out_file.to_token_stream().to_string()).unwrap();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-env=STAGELEFT_FINAL_CRATE_NAME={}", crate_name);

    println!(
        "cargo:rerun-if-changed={}",
        staged_path_absolute.to_string_lossy()
    );
}

struct GenFinalPubVistor {
    current_mod: Option<syn::Path>,
}

impl VisitMut for GenFinalPubVistor {
    fn visit_item_enum_mut(&mut self, i: &mut syn::ItemEnum) {
        i.vis = parse_quote!(pub);
        syn::visit_mut::visit_item_enum_mut(self, i);
    }

    fn visit_item_struct_mut(&mut self, i: &mut syn::ItemStruct) {
        i.vis = parse_quote!(pub);
        syn::visit_mut::visit_item_struct_mut(self, i);
    }

    fn visit_field_mut(&mut self, i: &mut syn::Field) {
        i.vis = parse_quote!(pub);
        syn::visit_mut::visit_field_mut(self, i);
    }

    fn visit_item_use_mut(&mut self, i: &mut syn::ItemUse) {
        i.vis = parse_quote!(pub);
        syn::visit_mut::visit_item_use_mut(self, i);
    }

    fn visit_item_mod_mut(&mut self, i: &mut syn::ItemMod) {
        let is_runtime = i
            .attrs
            .iter()
            .any(|a| a.path().to_token_stream().to_string() == "stageleft :: runtime");

        if is_runtime {
            *i = parse_quote! {
                #[cfg(feature = "macro")]
                #i
            };
        } else {
            let old_mod = self.current_mod.clone();
            let i_ident = &i.ident;
            self.current_mod = self
                .current_mod
                .as_ref()
                .map(|old_mod| parse_quote!(#old_mod::#i_ident));

            i.vis = parse_quote!(pub);

            syn::visit_mut::visit_item_mod_mut(self, i);

            self.current_mod = old_mod;
        }
    }

    fn visit_item_fn_mut(&mut self, i: &mut syn::ItemFn) {
        let is_entry = i
            .attrs
            .iter()
            .any(|a| a.path().to_token_stream().to_string() == "stageleft :: entry");

        if is_entry {
            *i = parse_quote! {
                #[cfg(feature = "macro")]
                #i
            }
        }

        i.vis = parse_quote!(pub);

        syn::visit_mut::visit_item_fn_mut(self, i);
    }

    fn visit_item_mut(&mut self, i: &mut syn::Item) {
        // TODO(shadaj): warn if a pub struct or enum has private fields
        // and is not marked for runtime
        if let Some(cur_path) = self.current_mod.as_ref() {
            if let syn::Item::Struct(s) = i {
                if matches!(s.vis, syn::Visibility::Public(_)) {
                    let e_name = &s.ident;
                    *i = parse_quote!(pub use #cur_path::#e_name;);
                    return;
                }
            } else if let syn::Item::Enum(e) = i {
                if matches!(e.vis, syn::Visibility::Public(_)) {
                    let e_name = &e.ident;
                    *i = parse_quote!(pub use #cur_path::#e_name;);
                    return;
                }
            }
        }

        syn::visit_mut::visit_item_mut(self, i);
    }

    fn visit_file_mut(&mut self, i: &mut syn::File) {
        i.items.retain(|i| match i {
            syn::Item::Macro(m) => {
                m.mac.path.to_token_stream().to_string() != "stageleft :: stageleft_crate"
            }
            _ => true,
        });

        syn::visit_mut::visit_file_mut(self, i);
    }
}

pub fn gen_final_helper(final_crate: &str) {
    let out_dir = env::var_os("OUT_DIR").unwrap();

    let mut flow_lib_pub = syn_inline_mod::parse_and_inline_modules(Path::new("src/lib.rs"));

    let mut final_pub_visitor = GenFinalPubVistor {
        current_mod: Some(parse_quote!(crate)),
    };
    final_pub_visitor.visit_file_mut(&mut flow_lib_pub);

    fs::write(
        Path::new(&out_dir).join("lib_pub.rs"),
        flow_lib_pub.to_token_stream().to_string(),
    )
    .unwrap();

    println!("cargo:rustc-env=STAGELEFT_FINAL_CRATE_NAME={}", final_crate);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
}

#[macro_export]
macro_rules! gen_final {
    () => {
        $crate::gen_final_helper(env!("CARGO_CRATE_NAME"))
    };
}
