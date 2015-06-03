use syntax::ast::{
    Ident,
    MetaItem,
    Item,
    Expr,
    StructDef,
    EnumDef,
};
use syntax::ast;
use syntax::codemap::Span;
use syntax::ext::base::ExtCtxt;
use syntax::ext::build::AstBuilder;
use syntax::ptr::P;

use aster;

use field;

pub fn expand_derive_deserialize(
    cx: &mut ExtCtxt,
    span: Span,
    _mitem: &MetaItem,
    item: &Item,
    push: &mut FnMut(P<ast::Item>)
) {
    let builder = aster::AstBuilder::new().span(span);

    let generics = match item.node {
        ast::ItemStruct(_, ref generics) => generics,
        ast::ItemEnum(_, ref generics) => generics,
        _ => cx.bug("expected ItemStruct or ItemEnum in #[derive(Deserialize)]")
    };

    let impl_generics = builder.from_generics(generics.clone())
        .add_ty_param_bound(
            builder.path().global().ids(&["serde", "de", "Deserialize"]).build()
        )
        .build();

    let ty = builder.ty().path()
        .segment(item.ident).with_generics(impl_generics.clone()).build()
        .build();

    let body = deserialize_body(
        cx,
        &builder,
        item,
        &impl_generics,
        ty.clone(),
    );

    let where_clause = &impl_generics.where_clause;

    let impl_item = quote_item!(cx,
        #[automatically_derived]
        impl $impl_generics ::serde::de::Deserialize for $ty $where_clause {
            fn deserialize<__D>(deserializer: &mut __D) -> ::std::result::Result<$ty, __D::Error>
                where __D: ::serde::de::Deserializer,
            {
                $body
            }
        }
    ).unwrap();

    push(impl_item)
}

fn deserialize_body(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    item: &Item,
    impl_generics: &ast::Generics,
    ty: P<ast::Ty>,
) -> P<ast::Expr> {
    match item.node {
        ast::ItemStruct(ref struct_def, _) => {
            deserialize_item_struct(
                cx,
                builder,
                item,
                impl_generics,
                ty,
                struct_def,
            )
        }
        ast::ItemEnum(ref enum_def, _) => {
            deserialize_item_enum(
                cx,
                builder,
                item.ident,
                impl_generics,
                ty,
                enum_def,
            )
        }
        _ => cx.bug("expected ItemStruct or ItemEnum in #[derive(Deserialize)]")
    }
}

fn deserialize_item_struct(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    item: &Item,
    impl_generics: &ast::Generics,
    ty: P<ast::Ty>,
    struct_def: &ast::StructDef,
) -> P<ast::Expr> {
    let mut named_fields = vec![];
    let mut unnamed_fields = 0;

    for field in struct_def.fields.iter() {
        match field.node.kind {
            ast::NamedField(name, _) => { named_fields.push(name); }
            ast::UnnamedField(_) => { unnamed_fields += 1; }
        }
    }

    match (named_fields.is_empty(), unnamed_fields == 0) {
        (true, true) => {
            deserialize_unit_struct(
                cx,
                &builder,
                item.ident,
            )
        }
        (true, false) => {
            deserialize_tuple_struct(
                cx,
                &builder,
                item.ident,
                impl_generics,
                ty,
                unnamed_fields,
            )
        }
        (false, true) => {
            deserialize_struct(
                cx,
                &builder,
                item.ident,
                impl_generics,
                ty,
                struct_def,
            )
        }
        (false, false) => {
            cx.bug("struct has named and unnamed fields")
        }
    }
}


// Build `__Visitor<A, B, ...>(PhantomData<A>, PhantomData<B>, ...)`
fn deserialize_visitor(
    builder: &aster::AstBuilder,
    trait_generics: &ast::Generics,
) -> (P<ast::Item>, P<ast::Ty>, P<ast::Expr>) {
    if trait_generics.ty_params.is_empty() {
        (
            builder.item().tuple_struct("__Visitor").build(),
            builder.ty().id("__Visitor"),
            builder.expr().id("__Visitor"),
        )
    } else {
        (
            builder.item().tuple_struct("__Visitor")
                .generics().with(trait_generics.clone()).build()
                .with_tys(
                    trait_generics.ty_params.iter().map(|ty_param| {
                        builder.ty().phantom_data().id(ty_param.ident)
                    })
                )
                .build(),
            builder.ty().path()
                .segment("__Visitor").with_generics(trait_generics.clone()).build()
                .build(),
            builder.expr().call().id("__Visitor")
                .with_args(
                    trait_generics.ty_params.iter().map(|_| {
                        builder.expr().phantom_data()
                    })
                )
                .build(),
        )
    }
}

fn deserialize_unit_struct(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    type_ident: Ident,
) -> P<ast::Expr> {
    let type_name = builder.expr().str(type_ident);

    quote_expr!(cx, {
        struct __Visitor;

        impl ::serde::de::Visitor for __Visitor {
            type Value = $type_ident;

            #[inline]
            fn visit_unit<E>(&mut self) -> ::std::result::Result<$type_ident, E>
                where E: ::serde::de::Error,
            {
                Ok($type_ident)
            }

            #[inline]
            fn visit_seq<V>(&mut self, mut visitor: V) -> ::std::result::Result<$type_ident, V::Error>
                where V: ::serde::de::SeqVisitor,
            {
                try!(visitor.end());
                self.visit_unit()
            }
        }

        deserializer.visit_named_unit($type_name, __Visitor)
    })
}

fn deserialize_tuple_struct(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    type_ident: Ident,
    impl_generics: &ast::Generics,
    ty: P<ast::Ty>,
    fields: usize,
) -> P<ast::Expr> {
    let where_clause = &impl_generics.where_clause;

    let (visitor_item, visitor_ty, visitor_expr) = deserialize_visitor(
        builder,
        impl_generics,
    );

    let visit_seq_expr = deserialize_seq(
        cx,
        builder,
        builder.path().id(type_ident).build(),
        fields,
    );

    let type_name = builder.expr().str(type_ident);

    quote_expr!(cx, {
        $visitor_item

        impl $impl_generics ::serde::de::Visitor for $visitor_ty $where_clause {
            type Value = $ty;

            fn visit_seq<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<$ty, __V::Error>
                where __V: ::serde::de::SeqVisitor,
            {
                $visit_seq_expr
            }
        }

        deserializer.visit_named_seq($type_name, $visitor_expr)
    })
}

fn deserialize_seq(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    struct_path: ast::Path,
    fields: usize,
) -> P<ast::Expr> {
    let let_values: Vec<P<ast::Stmt>> = (0 .. fields)
        .map(|i| {
            let name = builder.id(format!("__field{}", i));
            quote_stmt!(cx,
                let $name = match try!(visitor.visit()) {
                    Some(value) => value,
                    None => {
                        return Err(::serde::de::Error::end_of_stream_error());
                    }
                };
            ).unwrap()
        })
        .collect();

    let result = builder.expr().call()
        .build_path(struct_path)
        .with_args((0 .. fields).map(|i| builder.expr().id(format!("__field{}", i))))
        .build();

    quote_expr!(cx, {
        $let_values

        try!(visitor.end());

        Ok($result)
    })
}

fn deserialize_struct(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    type_ident: Ident,
    impl_generics: &ast::Generics,
    ty: P<ast::Ty>,
    struct_def: &StructDef,
) -> P<ast::Expr> {
    let where_clause = &impl_generics.where_clause;

    let (visitor_item, visitor_ty, visitor_expr) = deserialize_visitor(
        builder,
        impl_generics,
    );

    let (field_visitor, visit_map_expr) = deserialize_struct_visitor(
        cx,
        builder,
        struct_def,
        builder.path().id(type_ident).build(),
    );

    let type_name = builder.expr().str(type_ident);

    quote_expr!(cx, {
        $field_visitor

        $visitor_item

        impl $impl_generics ::serde::de::Visitor for $visitor_ty $where_clause {
            type Value = $ty;

            #[inline]
            fn visit_map<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<$ty, __V::Error>
                where __V: ::serde::de::MapVisitor,
            {
                $visit_map_expr
            }
        }

        deserializer.visit_named_map($type_name, $visitor_expr)
    })
}

fn deserialize_item_enum(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    type_ident: Ident,
    impl_generics: &ast::Generics,
    ty: P<ast::Ty>,
    enum_def: &EnumDef,
) -> P<ast::Expr> {
    let where_clause = &impl_generics.where_clause;

    let type_name = builder.expr().str(type_ident);

    let variant_visitor = deserialize_field_visitor(
        cx,
        builder,
        enum_def.variants.iter()
            .map(|variant| builder.expr().str(variant.node.name))
            .collect()
    );

    // Match arms to extract a variant from a string
    let variant_arms: Vec<_> = enum_def.variants.iter()
        .enumerate()
        .map(|(i, variant)| {
            let variant_name = builder.expr().path()
                .id("__Field").id(format!("__field{}", i))
                .build();

            let expr = deserialize_variant(
                cx,
                builder,
                type_ident,
                impl_generics,
                ty.clone(),
                variant,
            );

            quote_arm!(cx, $variant_name => { $expr })
        })
        .collect();

    let (visitor_item, visitor_ty, visitor_expr) = deserialize_visitor(
        builder,
        impl_generics,
    );

    quote_expr!(cx, {
        $variant_visitor

        $visitor_item

        impl $impl_generics ::serde::de::EnumVisitor for $visitor_ty $where_clause {
            type Value = $ty;

            fn visit<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<$ty, __V::Error>
                where __V: ::serde::de::VariantVisitor,
            {
                match try!(visitor.visit_variant()) {
                    $variant_arms
                }
            }
        }

        deserializer.visit_enum($type_name, $visitor_expr)
    })
}

fn deserialize_variant(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    type_ident: Ident,
    generics: &ast::Generics,
    ty: P<ast::Ty>,
    variant: &ast::Variant,
) -> P<ast::Expr> {
    let variant_ident = variant.node.name;

    match variant.node.kind {
        ast::TupleVariantKind(ref args) if args.is_empty() => {
            quote_expr!(cx, {
                try!(visitor.visit_unit());
                Ok($type_ident::$variant_ident)
            })
        }
        ast::TupleVariantKind(ref args) => {
            deserialize_tuple_variant(
                cx,
                builder,
                type_ident,
                variant_ident,
                generics,
                ty,
                args.len(),
            )
        }
        ast::StructVariantKind(ref struct_def) => {
            deserialize_struct_variant(
                cx,
                builder,
                type_ident,
                variant_ident,
                generics,
                ty,
                struct_def,
            )
        }
    }
}

fn deserialize_tuple_variant(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    type_ident: ast::Ident,
    variant_ident: ast::Ident,
    generics: &ast::Generics,
    ty: P<ast::Ty>,
    fields: usize,
) -> P<ast::Expr> {
    let where_clause = &generics.where_clause;

    let (visitor_item, visitor_ty, visitor_expr) = deserialize_visitor(
        builder,
        generics,
    );

    let visit_seq_expr = deserialize_seq(
        cx,
        builder,
        builder.path().id(type_ident).id(variant_ident).build(),
        fields,
    );

    quote_expr!(cx, {
        $visitor_item

        impl $generics ::serde::de::Visitor for $visitor_ty $where_clause {
            type Value = $ty;

            fn visit_seq<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<$ty, __V::Error>
                where __V: ::serde::de::SeqVisitor,
            {
                $visit_seq_expr
            }
        }

        visitor.visit_seq($visitor_expr)
    })
}

fn deserialize_struct_variant(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    type_ident: ast::Ident,
    variant_ident: ast::Ident,
    generics: &ast::Generics,
    ty: P<ast::Ty>,
    struct_def: &ast::StructDef,
) -> P<ast::Expr> {
    let where_clause = &generics.where_clause;

    let (field_visitor, field_expr) = deserialize_struct_visitor(
        cx,
        builder,
        struct_def,
        builder.path().id(type_ident).id(variant_ident).build(),
    );

    let (visitor_item, visitor_ty, visitor_expr) = deserialize_visitor(
        builder,
        generics,
    );

    quote_expr!(cx, {
        $field_visitor

        $visitor_item

        impl $generics ::serde::de::Visitor for $visitor_ty $where_clause {
            type Value = $ty;

            fn visit_map<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<$ty, __V::Error>
                where __V: ::serde::de::MapVisitor,
            {
                $field_expr
            }
        }

        visitor.visit_map($visitor_expr)
    })
}

fn deserialize_field_visitor(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    field_exprs: Vec<P<ast::Expr>>,
) -> Vec<P<ast::Item>> {
    // Create the field names for the fields.
    let field_idents: Vec<ast::Ident> = (0 .. field_exprs.len())
        .map(|i| builder.id(format!("__field{}", i)))
        .collect();

    let field_enum = builder.item()
        .attr().allow(&["non_camel_case_types"])
        .enum_("__Field")
        .with_variants(
            field_idents.iter().map(|field_ident| {
                builder.variant(field_ident).tuple().build()
            })
        )
        .build();

    // Match arms to extract a field from a string
    let field_arms: Vec<_> = field_idents.iter()
        .zip(field_exprs.into_iter())
        .map(|(field_ident, field_expr)| {
            quote_arm!(cx, $field_expr => { Ok(__Field::$field_ident) })
        })
        .collect();

    vec![
        field_enum,

        quote_item!(cx,
            impl ::serde::de::Deserialize for __Field {
                #[inline]
                fn deserialize<D>(deserializer: &mut D) -> ::std::result::Result<__Field, D::Error>
                    where D: ::serde::de::Deserializer,
                {
                    struct __FieldVisitor;

                    impl ::serde::de::Visitor for __FieldVisitor {
                        type Value = __Field;

                        fn visit_str<E>(&mut self, value: &str) -> ::std::result::Result<__Field, E>
                            where E: ::serde::de::Error,
                        {
                            match value {
                                $field_arms
                                _ => Err(::serde::de::Error::unknown_field_error(value)),
                            }
                        }
                    }

                    deserializer.visit(__FieldVisitor)
                }
            }
        ).unwrap(),
    ]
}

fn deserialize_struct_visitor(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    struct_def: &ast::StructDef,
    struct_path: ast::Path,
) -> (Vec<P<ast::Item>>, P<ast::Expr>) {
    let field_visitor = deserialize_field_visitor(
        cx,
        builder,
        field::struct_field_strs(cx, builder, struct_def, field::Direction::Deserialize),
    );

    let visit_map_expr = deserialize_map(
        cx,
        builder,
        struct_path,
        struct_def,
    );

    (field_visitor, visit_map_expr)
}

fn deserialize_map(
    cx: &ExtCtxt,
    builder: &aster::AstBuilder,
    struct_path: ast::Path,
    struct_def: &StructDef,
) -> P<ast::Expr> {
    // Create the field names for the fields.
    let field_names: Vec<ast::Ident> = (0 .. struct_def.fields.len())
        .map(|i| builder.id(format!("__field{}", i)))
        .collect();

    // Declare each field.
    let let_values: Vec<P<ast::Stmt>> = field_names.iter()
        .map(|field_name| quote_stmt!(cx, let mut $field_name = None;).unwrap())
        .collect();

    // Match arms to extract a value for a field.
    let value_arms: Vec<ast::Arm> = field_names.iter()
        .map(|field_name| {
            quote_arm!(cx,
                __Field::$field_name => {
                    $field_name = Some(try!(visitor.visit_value()));
                }
            )
        })
        .collect();

    let extract_values: Vec<P<ast::Stmt>> = field_names.iter()
        .zip(struct_def.fields.iter())
        .map(|(field_name, field)| {
            let rename = field::field_rename(field, &field::Direction::Deserialize);
            let name_str = match (rename, field.node.kind) {
                (Some(rename), _) => builder.expr().build_lit(P(rename.clone())),
                (None, ast::NamedField(name, _)) => builder.expr().str(name),
                (None, ast::UnnamedField(_)) => panic!("struct contains unnamed fields"),
            };

            let missing_expr = if field::default_value(field) {
                quote_expr!(cx, ::std::default::Default::default())
            } else {
                quote_expr!(cx, try!(visitor.missing_field($name_str)))
            };

            quote_stmt!(cx,
                let $field_name = match $field_name {
                    Some($field_name) => $field_name,
                    None => $missing_expr,
                };
            ).unwrap()
        })
        .collect();

    let result = builder.expr().struct_path(struct_path)
        .with_id_exprs(
            struct_def.fields.iter()
                .zip(field_names.iter())
                .map(|(field, field_name)| {
                    (
                        match field.node.kind {
                            ast::NamedField(name, _) => name.clone(),
                            ast::UnnamedField(_) => panic!("struct contains unnamed fields"),
                        },
                        builder.expr().id(field_name),
                    )
                })
        )
        .build();

    quote_expr!(cx, {
        $let_values

        while let Some(key) = try!(visitor.visit_key()) {
            match key {
                $value_arms
            }
        }

        $extract_values

        try!(visitor.end());

        Ok($result)
    })
}
