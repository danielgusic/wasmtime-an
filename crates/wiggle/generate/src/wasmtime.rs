use crate::CodegenSettings;
use crate::config::Asyncness;
use crate::funcs::func_bounds;
use crate::names;
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};
use std::collections::HashSet;

pub fn link_module(
    module: &witx::Module,
    target_path: Option<&syn::Path>,
    settings: &CodegenSettings,
) -> TokenStream {
    let module_ident = names::module(&module.name);

    let send_bound = if settings.async_.contains_async(module) {
        quote! { + Send, T: Send }
    } else {
        quote! {}
    };

    let mut bodies = Vec::new();
    let mut bounds = HashSet::new();
    for f in module.funcs() {
        let asyncness = settings.async_.get(module.name.as_str(), f.name.as_str());
        bodies.push(generate_func(&module, &f, target_path, asyncness));
        let bound = func_bounds(module, &f, settings);
        for b in bound {
            bounds.insert(b);
        }
    }

    let ctx_bound = if let Some(target_path) = target_path {
        let bounds = bounds
            .into_iter()
            .map(|b| quote!(#target_path::#module_ident::#b));
        quote!( #(#bounds)+* #send_bound )
    } else {
        let bounds = bounds.into_iter();
        quote!( #(#bounds)+* #send_bound )
    };

    let func_name = if target_path.is_none() {
        format_ident!("add_to_linker")
    } else {
        format_ident!("add_{}_to_linker", module_ident)
    };

    let u = if settings.mutable {
        quote!(&mut U)
    } else {
        quote!(&U)
    };
    quote! {
        /// Adds all instance items to the specified `Linker`.
        pub fn #func_name<T, U>(
            linker: &mut wiggle::wasmtime_crate::Linker<T>,
            get_cx: impl Fn(&mut T) -> #u + Send + Sync + Copy + 'static,
        ) -> wiggle::error::Result<()>
            where
                T: 'static,
                U: #ctx_bound #send_bound
        {
            #(#bodies)*
            Ok(())
        }
    }
}

fn generate_func(
    module: &witx::Module,
    func: &witx::InterfaceFunc,
    target_path: Option<&syn::Path>,
    asyncness: Asyncness,
) -> TokenStream {
    let module_str = module.name.as_str();
    let module_ident = names::module(&module.name);

    let field_str = func.name.as_str();
    let field_ident = names::func(&func.name);

    let (params, results) = func.wasm_signature();

    let arg_names = (0..params.len())
        .map(|i| Ident::new(&format!("arg{i}"), Span::call_site()))
        .collect::<Vec<_>>();
    let arg_tys = params
        .iter()
        .map(|ty| names::wasm_type(*ty))
        .collect::<Vec<_>>();
    let arg_decls = arg_names
        .iter()
        .zip(arg_tys.iter())
        .map(|(name, ty)| {
            quote! { #name: #ty }
        })
        .collect::<Vec<_>>();

    let ret_ty = match results.len() {
        0 => quote!(()),
        1 => names::wasm_type(results[0]),
        _ => unimplemented!(),
    };

    let await_ = if asyncness.is_sync() {
        quote!()
    } else {
        quote!(.await)
    };

    let abi_func = if let Some(target_path) = target_path {
        quote!( #target_path::#module_ident::#field_ident )
    } else {
        quote!( #field_ident )
    };

    let body = quote! {
        let export = caller.get_export("memory");
        let fuel = wiggle::wasmtime_crate::AsContextMut::as_context_mut(&mut caller).hostcall_fuel();
        // Keep a copy of the memory handle so the AN-encoding shadow can be
        // re-encoded for host-written ranges after the hostcall body runs.
        let an_memory = match &export {
            Some(wiggle::wasmtime_crate::Extern::Memory(m)) => Some(*m),
            _ => None,
        };
        let (mut mem, ctx) = match &export {
            Some(wiggle::wasmtime_crate::Extern::Memory(m)) => {
                // When this memory has an AN-encoding shadow, hand the
                // `GuestMemory` view the shadow + constant so host *reads*
                // verify exactly the range they touch (verify-at-use, never the
                // whole memory) and record host *write* ranges (re-encoded
                // below). The untracked accessor skips the whole-dirty mark that
                // `data_and_store_mut` would set.
                let (raw, shadow, a, ctx) =
                    m.an_untracked_data_shadow_and_store_mut(&mut caller);
                let ctx = get_cx(ctx);
                ctx.set_hostcall_fuel(fuel);
                let mem = match shadow {
                    Some(shadow) => wiggle::GuestMemory::unshared_an_verified(raw, shadow, a),
                    None => wiggle::GuestMemory::unshared(raw),
                };
                (mem, ctx)
            }
            Some(wiggle::wasmtime_crate::Extern::SharedMemory(m)) => {
                let ctx = get_cx(caller.data_mut());
                ctx.set_hostcall_fuel(fuel);
                (wiggle::GuestMemory::shared(m.data()), ctx)
            }
            _ => wiggle::error::bail!("missing required memory export"),
        };
        let result = #abi_func(ctx, &mut mem #(, #arg_names)*) #await_;
        // Bring the AN-encoding shadow back in sync for exactly the ranges
        // the host wrote. This must run on the error path too: writes may
        // have landed before the error. No-op when tracking is off.
        let an_dirty = mem.an_take_dirty();
        if let Some(m) = an_memory {
            for r in an_dirty {
                // `false`: a partially covered boundary slot's retained bytes
                // disagree with the shadow — pre-existing corruption the
                // re-encode would otherwise launder.
                if !m.an_resync_range(&mut caller, r.start as usize, (r.end - r.start) as usize) {
                    wiggle::error::bail!("AN-encoding memory mismatch at hostcall write resync");
                }
            }
        }
        Ok(<#ret_ty>::from(result?))
    };

    match asyncness {
        Asyncness::Async => {
            let arg_decls = quote! { ( #(#arg_names,)* ) : ( #(#arg_tys,)* ) };
            quote! {
                linker.func_wrap_async(
                    #module_str,
                    #field_str,
                    move |mut caller: wiggle::wasmtime_crate::Caller<'_, T>, #arg_decls| {
                        Box::new(async move { #body })
                    },
                )?;
            }
        }

        Asyncness::Blocking { block_with } => {
            quote! {
                linker.func_wrap(
                    #module_str,
                    #field_str,
                    move |mut caller: wiggle::wasmtime_crate::Caller<'_, T> #(, #arg_decls)*| -> wiggle::error::Result<#ret_ty> {
                        let result = async { #body };
                        #block_with(result)?
                    },
                )?;
            }
        }

        Asyncness::Sync => {
            quote! {
                linker.func_wrap(
                    #module_str,
                    #field_str,
                    move |mut caller: wiggle::wasmtime_crate::Caller<'_, T> #(, #arg_decls)*| -> wiggle::error::Result<#ret_ty> {
                        #body
                    },
                )?;
            }
        }
    }
}
