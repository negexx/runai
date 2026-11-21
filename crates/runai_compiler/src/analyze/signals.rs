//! Which `const`s in a file are reactive accessors, and which calls resolve to them.
//!
//! Every rule in this milestone that talks about "a signal" has to answer the same
//! question first: *is this call a primitive, or an accessor?* The answer cannot be
//! a name comparison. `signal` is an ordinary identifier, so a local function, a
//! parameter, or a nested `const signal = …` shadows the import; conversely,
//! `import { signal as createSignal }` makes a primitive whose name never appears
//! in the call site. The only reliable answer comes from `oxc_semantic`: a call is a
//! **primitive call** when its callee resolves to the symbol of a named import of
//! `signal`, `derived`, `effect`, or `resource` from `"runai"`, and it is an
//! **accessor call** when its callee resolves to the symbol of a binding created by
//! a `signal`/`derived` primitive call.
//!
//! # Why the resolved facts are stored, not recomputed
//!
//! Resolving a callee needs `Scoping`, and building it is the expensive half of
//! analysis. [`find_signal_bindings`] therefore builds the semantic data **once**
//! and stores everything the later rules need as plain spans and symbol ids: which
//! accessors exist, which calls read or write them, how those calls sit relative to
//! the nearest function and to a JSX expression container, and which spans are
//! `effect` scopes. [`check_signal_rules`] then answers the rules from that record
//! alone.
//!
//! [`check_signal_rules`]: crate::analyze::rules::check_signal_rules
//!
//! # Identity, not names
//!
//! [`SignalBindings::accessors`] carries the name and the declaration span, because
//! a diagnostic has to name the accessor a reader can find. The *identity* the rules
//! compare is [`SignalBindings::accessor_symbols`], index-aligned with it: the
//! `oxc_semantic` symbol each binding resolves to. Two same-named bindings in
//! different scopes therefore never match each other, and a symbol id is the only
//! thing that can be compared safely.

use oxc_ast::ast::{
    Argument, ArrowFunctionBody, ArrowFunctionExpression, CallExpression, Expression, Function,
    ImportDeclarationSpecifier, ImportOrExportKind, Program, Statement, VariableDeclarator,
};
use oxc_ast_visit::Visit;
use oxc_semantic::{Scoping, SemanticBuilder, SymbolId};
use oxc_span::GetSpan;
use oxc_syntax::scope::ScopeFlags;

/// The module every reactive primitive must be imported from.
const PRIMITIVE_SOURCE: &str = "runai";

/// A reactive primitive `"runai"` exports.
///
/// The enum exists so "which primitives bind an accessor" and "which primitives
/// take a function that runs in a tracking scope" each have **one** answer in the
/// crate. A new primitive is a new arm here, not another list of names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Primitive {
    Signal,
    Derived,
    Effect,
    Resource,
}

impl Primitive {
    /// The primitive `name` names, as it is *imported* (`{ signal as … }` still
    /// imports `signal`), or `None` for any other identifier.
    fn imported(name: &str) -> Option<Self> {
        match name {
            "signal" => Some(Self::Signal),
            "derived" => Some(Self::Derived),
            "effect" => Some(Self::Effect),
            "resource" => Some(Self::Resource),
            _ => None,
        }
    }

    /// Whether `const x = <primitive>(…)` creates an accessor, i.e. whether `x()` is
    /// a read and `x(v)` is a write.
    fn binds_accessor(self) -> bool {
        matches!(self, Self::Signal | Self::Derived)
    }
}

/// One accessor call — a read (`count()`) or a write (`count(next)`).
///
/// The call is resolved in [`find_signal_bindings`], where the semantic data is
/// available; the rules only compare the recorded spans afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessorCall {
    /// The span of the whole call, `count()` or `count(next)` included.
    ///
    /// The whole call, not just the callee, because that is the construct the
    /// diagnostic names and what `crate::diagnostic`'s own sample spans.
    pub span: (u32, u32),
    /// The accessor's binding name, for the diagnostic message.
    pub name: String,
    /// `true` for `count(next)`, `false` for `count()`.
    pub write: bool,
    /// The body span of the innermost enclosing function or arrow, in the same
    /// representation [`crate::analyze::component`] uses for `body_span`.
    ///
    /// `None` at module level. Comparing this with a component's `body_span` is how
    /// the rules ask "is the nearest function the component?", and comparing it with
    /// [`SignalBindings::effect_scopes`] is how they ask "is it the function passed
    /// to an effect?".
    pub nearest_function: Option<(u32, u32)>,
    /// Whether a `JSXExpressionContainer` lies between the call and that function.
    ///
    /// This is the tracking-scope half of `RA001`: a read in a JSX slot is owned by
    /// the slot, so nothing is frozen.
    pub in_jsx_container: bool,
}

/// The reactive bindings of one file, plus the resolved call facts the signal rules
/// compare against.
///
/// The first two fields are the plan's interface. The rest is the extension this
/// task needs: every fact below requires `oxc_semantic` to resolve, and the plan
/// requires building that data once rather than once per rule. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalBindings {
    /// Every accessor the file declares, as `(binding name, declaration span)`.
    pub accessors: Vec<(String, (u32, u32))>,
    /// The subset created by `derived(…)`, by binding name.
    pub derived_names: Vec<String>,
    /// The `oxc_semantic` symbol of each entry in [`Self::accessors`], same order.
    pub accessor_symbols: Vec<SymbolId>,
    /// Every call whose callee resolves to one of [`Self::accessor_symbols`].
    pub accessor_calls: Vec<AccessorCall>,
    /// The span of every primitive call (`signal(…)`, `derived(…)`, `effect(…)`,
    /// `resource(…)`), however it is used. `RA010` asks whether one exists.
    pub primitive_calls: Vec<(u32, u32)>,
    /// The body span of the function passed to every `effect(…)` call.
    pub effect_scopes: Vec<(u32, u32)>,
}

/// Find the file's accessors and resolve its primitive and accessor calls.
///
/// One semantic build. `oxc_semantic` is not optional here: the whole point is that
/// the answer survives shadowing and aliasing, which only symbol resolution can do.
/// A local function named `signal`, an unimported `signal(…)`, and a `signal(…)`
/// imported from any module other than `"runai"` are deliberately not primitives —
/// the first two are `tsc`'s business once the type-check plan lands.
pub fn find_signal_bindings<'a>(program: &'a Program<'a>) -> SignalBindings {
    let semantic = SemanticBuilder::new().build(program).semantic;
    let scoping = semantic.scoping();
    let primitives = import_symbols(program);

    let mut accessors = AccessorCollector {
        scoping,
        primitives: &primitives,
        accessors: Vec::new(),
    };
    accessors.visit_program(program);
    let declared = accessors.accessors;

    let mut calls = CallCollector {
        scoping,
        primitives: &primitives,
        accessors: &declared,
        calls: Vec::new(),
        primitive_calls: Vec::new(),
        effect_scopes: Vec::new(),
        functions: Vec::new(),
        container_floor: Vec::new(),
        containers: 0,
    };
    calls.visit_program(program);

    SignalBindings {
        accessors: declared
            .iter()
            .map(|accessor| (accessor.name.clone(), accessor.span))
            .collect(),
        derived_names: declared
            .iter()
            .filter(|accessor| accessor.derived)
            .map(|accessor| accessor.name.clone())
            .collect(),
        accessor_symbols: declared.iter().map(|accessor| accessor.symbol).collect(),
        accessor_calls: calls.calls,
        primitive_calls: calls.primitive_calls,
        effect_scopes: calls.effect_scopes,
    }
}

/// An accessor binding found by [`AccessorCollector`].
struct Accessor {
    /// The `const`'s binding name, e.g. `count`.
    name: String,
    /// The binding identifier's span, for a message that points at the declaration.
    span: (u32, u32),
    /// The `oxc_semantic` symbol the binding resolves to.
    symbol: SymbolId,
    /// Whether `derived(…)` created it rather than `signal(…)`.
    derived: bool,
}

/// Collect `const x = signal(…)` and `const y = derived(…)` where the callee resolves
/// to a `"runai"` import.
///
/// Any nesting: the walk reaches `const`s in functions and blocks alike, because an
/// accessor created inside a component body is exactly the common case.
///
/// Any *declaration kind* too — `let`/`var` are collected although the plan writes
/// `const`. This is deliberate, not an oversight: an accessor bound by `let` is the same
/// reactive value at the same symbol, so skipping it would make `RA001`/`RA009` depend
/// on a style detail the plan only expresses as convention.
struct AccessorCollector<'s> {
    scoping: &'s Scoping,
    primitives: &'s [(SymbolId, Primitive)],
    accessors: Vec<Accessor>,
}

impl<'a> Visit<'a> for AccessorCollector<'_> {
    fn visit_variable_declarator(&mut self, declarator: &VariableDeclarator<'a>) {
        if let Some(accessor) = self.accessor(declarator) {
            self.accessors.push(accessor);
        }
        oxc_ast_visit::walk::walk_variable_declarator(self, declarator);
    }
}

impl AccessorCollector<'_> {
    /// The accessor `declarator` binds, if its initializer is a primitive call.
    fn accessor(&self, declarator: &VariableDeclarator<'_>) -> Option<Accessor> {
        let Some(Expression::CallExpression(call)) = declarator.init.as_ref() else {
            return None;
        };
        let primitive = self.primitive_of(call)?;
        if !primitive.binds_accessor() {
            return None;
        }
        // `const { x } = signal(…)` binds no single name this rule can point at.
        let binding = declarator.id.get_binding_identifier()?;
        Some(Accessor {
            name: binding.name.as_str().to_string(),
            span: (binding.span.start, binding.span.end),
            symbol: binding.symbol_id.get()?,
            derived: primitive == Primitive::Derived,
        })
    }

    /// The primitive `call`'s callee resolves to, if it is one.
    fn primitive_of(&self, call: &CallExpression<'_>) -> Option<Primitive> {
        let symbol = resolve_callee(call, self.scoping)?;
        self.primitives
            .iter()
            .find(|(primitive, _)| *primitive == symbol)
            .map(|(_, primitive)| *primitive)
    }
}

/// Resolve every primitive and accessor call in the file.
///
/// The visitor maintains its own function and JSX-container stack rather than
/// climbing semantic ancestor nodes: the walk already visits every node once, and a
/// stack is both cheaper and independent of whether the semantic builder recorded a
/// full node store.
struct CallCollector<'s> {
    scoping: &'s Scoping,
    primitives: &'s [(SymbolId, Primitive)],
    accessors: &'s [Accessor],
    calls: Vec<AccessorCall>,
    primitive_calls: Vec<(u32, u32)>,
    effect_scopes: Vec<(u32, u32)>,
    /// Body spans of the enclosing functions, innermost last.
    functions: Vec<(u32, u32)>,
    /// How many `JSXExpressionContainer`s enclose the node being visited.
    containers: usize,
    /// The container depth when each entry of [`Self::functions`] was entered, so a
    /// call is "in a JSX slot" only when a container sits *inside* that function.
    container_floor: Vec<usize>,
}

impl<'a> Visit<'a> for CallCollector<'_> {
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Some(symbol) = resolve_callee(call, self.scoping) {
            self.record(symbol, call);
        }
        oxc_ast_visit::walk::walk_call_expression(self, call);
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        self.enter_function(function.body.as_ref().map_or_else(
            || (function.span.start, function.span.end),
            |body| (body.span.start, body.span.end),
        ));
        oxc_ast_visit::walk::walk_function(self, function, flags);
        self.leave_function();
    }

    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        self.enter_function(match &arrow.body {
            ArrowFunctionBody::FunctionBody(body) => (body.span.start, body.span.end),
            concise => {
                let span = concise.span();
                (span.start, span.end)
            }
        });
        oxc_ast_visit::walk::walk_arrow_function_expression(self, arrow);
        self.leave_function();
    }

    fn visit_jsx_expression_container(
        &mut self,
        container: &oxc_ast::ast::JSXExpressionContainer<'a>,
    ) {
        self.containers += 1;
        oxc_ast_visit::walk::walk_jsx_expression_container(self, container);
        self.containers -= 1;
    }
}

impl CallCollector<'_> {
    /// Push `body` as the nearest function.
    fn enter_function(&mut self, body: (u32, u32)) {
        self.functions.push(body);
        self.container_floor.push(self.containers);
    }

    /// Pop the nearest function.
    fn leave_function(&mut self) {
        self.functions.pop();
        self.container_floor.pop();
    }

    /// The body span of the innermost enclosing function, if there is one.
    fn nearest_function(&self) -> Option<(u32, u32)> {
        self.functions.last().copied()
    }

    /// Whether a JSX container was entered inside the nearest function.
    fn in_jsx_container(&self) -> bool {
        self.container_floor
            .last()
            .is_some_and(|floor| self.containers > *floor)
    }

    /// Record `call` if its callee is an accessor or a primitive.
    fn record(&mut self, symbol: SymbolId, call: &CallExpression<'_>) {
        // A primitive is a primitive *however it is called*: `effect(fn, opts)` and
        // `resource(fn, { … })` are still primitive call sites, and the arity gate
        // below is about the *accessor* call shapes `RA001`/`RA009` define. Recording
        // the primitive first keeps [`SignalBindings::primitive_calls`] complete, which
        // is what `RA010` will read.
        if let Some((_, primitive)) = self
            .primitives
            .iter()
            .find(|(primitive, _)| *primitive == symbol)
        {
            self.primitive_calls.push((call.span.start, call.span.end));
            // `effect` is the only scope `RA002` asks about: a write whose nearest
            // function is this one makes the write a dependency of its own run.
            if *primitive == Primitive::Effect
                && let Some(scope) = argument_function_body(call)
            {
                self.effect_scopes.push(scope);
            }
            return;
        }

        // A read is `x()`, a write is `x(v)`. Any other arity is not an accessor use
        // this milestone defines, and guessing at it would report a call shape the
        // design does not have.
        let write = match call.arguments.len() {
            0 => false,
            1 => true,
            _ => return,
        };

        if let Some(accessor) = self.accessors.iter().find(|it| it.symbol == symbol) {
            self.calls.push(AccessorCall {
                span: (call.span.start, call.span.end),
                name: accessor.name.clone(),
                write,
                nearest_function: self.nearest_function(),
                in_jsx_container: self.in_jsx_container(),
            });
        }
    }
}

/// The symbols of the named imports of the reactive primitives from `"runai"`.
///
/// `import { signal as createSignal }` records the symbol of `createSignal`, so
/// every later question is about identity. `import type { signal }` is skipped:
/// types are stripped, so it cannot be a runtime primitive.
fn import_symbols(program: &Program<'_>) -> Vec<(SymbolId, Primitive)> {
    let mut symbols = Vec::new();
    for statement in &program.body {
        let Statement::ImportDeclaration(declaration) = statement else {
            continue;
        };
        if declaration.source.value.as_str() != Some(PRIMITIVE_SOURCE) {
            continue;
        }
        if declaration.import_kind == ImportOrExportKind::Type {
            continue;
        }
        let Some(specifiers) = &declaration.specifiers else {
            continue;
        };
        for specifier in specifiers {
            let ImportDeclarationSpecifier::ImportSpecifier(specifier) = specifier else {
                continue;
            };
            if specifier.import_kind == ImportOrExportKind::Type {
                continue;
            }
            let Some(primitive) = Primitive::imported(specifier.imported.name().as_str()) else {
                continue;
            };
            if let Some(symbol) = specifier.local.symbol_id.get() {
                symbols.push((symbol, primitive));
            }
        }
    }
    symbols
}

/// The symbol `call`'s callee identifier resolves to, or `None`.
///
/// Only a plain identifier callee can be a primitive or an accessor: `runai.signal(0)`
/// and `foo.signal(0)` are member calls, and this milestone has no namespace import
/// form to resolve them against.
fn resolve_callee(call: &CallExpression<'_>, scoping: &Scoping) -> Option<SymbolId> {
    let Expression::Identifier(identifier) = &call.callee else {
        return None;
    };
    let reference = identifier.reference_id.get()?;
    scoping.get_reference(reference).symbol_id()
}

/// The body span of `call`'s first argument when it is a function, or `None`.
///
/// `effect(() => { … })` is the tracking scope even when the arrow is parenthesized,
/// which is how a multi-line effect is normally formatted. The body span, not the
/// function's own span, is what [`AccessorCall::nearest_function`] records, so the
/// two are directly comparable.
fn argument_function_body(call: &CallExpression<'_>) -> Option<(u32, u32)> {
    let argument = match call.arguments.first() {
        Some(Argument::SpreadElement(_)) | None => return None,
        Some(argument) => argument.to_expression(),
    };
    function_body(argument)
}

/// The body span of `expression` when it *is* a function, seeing through parentheses.
fn function_body(expression: &Expression<'_>) -> Option<(u32, u32)> {
    match expression {
        Expression::ArrowFunctionExpression(arrow) => Some(match &arrow.body {
            ArrowFunctionBody::FunctionBody(body) => (body.span.start, body.span.end),
            concise => {
                let span = concise.span();
                (span.start, span.end)
            }
        }),
        Expression::FunctionExpression(function) => Some(
            function
                .body
                .as_ref()
                .map_or((function.span.start, function.span.end), |body| {
                    (body.span.start, body.span.end)
                }),
        ),
        Expression::ParenthesizedExpression(parenthesized) => {
            function_body(&parenthesized.expression)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use oxc_allocator::Allocator;

    use super::*;
    use crate::source::parse_tsx;

    fn bindings(src: &str) -> SignalBindings {
        let alloc = Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        find_signal_bindings(program)
    }

    #[test]
    fn a_signal_binding_is_an_accessor() {
        let src = "import { signal } from \"runai\"\nconst n = signal(0)\n";
        let b = bindings(src);
        assert_eq!(b.accessors.len(), 1);
        assert_eq!(b.accessors[0].0, "n");
        // The recorded span is the binding identifier, which is where a message
        // pointing at the declaration would lead.
        let (start, end) = b.accessors[0].1;
        assert_eq!(&src[start as usize..end as usize], "n");
        assert!(b.derived_names.is_empty());
        assert_eq!(b.accessor_symbols.len(), 1);
    }

    #[test]
    fn a_derived_binding_is_an_accessor_and_is_named() {
        let b = bindings(
            "import { signal, derived } from \"runai\"\nconst n = signal(0)\nconst d = derived(() => n() + 1)\n",
        );
        assert_eq!(b.derived_names, vec!["d".to_string()]);
        assert_eq!(b.accessors.len(), 2);
    }

    /// Aliases are resolved by the *imported* name, so `createSignal` is a primitive
    /// even though the word `signal` appears nowhere at the call site.
    #[test]
    fn an_aliased_import_is_a_primitive() {
        let b = bindings(
            "import { signal as createSignal } from \"runai\"\nconst n = createSignal(0)\n",
        );
        assert_eq!(b.accessors.len(), 1);
        assert_eq!(b.accessors[0].0, "n");
    }

    /// A local function with the primitive's name is not the import.
    ///
    /// A *module-level* `const signal = …` beside `import { signal }` is a JS
    /// redeclaration error, and `tsc` reports it, so it is not a shadow this rule
    /// has to answer. The shape that matters is a shadow in an inner scope, which is
    /// what an agent writes when a helper takes a `signal` parameter.
    #[test]
    fn a_shadowed_name_is_not_a_primitive() {
        let b = bindings(
            "import { signal } from \"runai\"\n\
             const signal2 = (v: number) => () => v\n\
             function P() { const signal = signal2; const n = signal(0); return <p/> }\n",
        );
        assert!(b.accessors.is_empty(), "{b:?}");
        assert!(b.primitive_calls.is_empty(), "{b:?}");
    }

    /// The same fixture reads as a primitive once the shadow is gone, so the test
    /// above fails for the shadow and not for a fixture that never resolved.
    #[test]
    fn the_unshadowed_fixture_is_a_primitive() {
        let b = bindings(
            "import { signal } from \"runai\"\n\
             const signal2 = (v: number) => () => v\n\
             function P() { const n = signal(0); return <p/> }\n",
        );
        assert_eq!(b.accessors.len(), 1, "{b:?}");
    }

    /// Calls are matched by symbol, not by name.
    ///
    /// This is the fixture a name comparison gets wrong in both directions: `n` is an
    /// accessor, and a *different* `n` in an inner block is an ordinary function
    /// called with one argument. Comparing names would resolve the call to the
    /// accessor and report a write that does not exist; comparing symbols does not.
    #[test]
    fn a_call_resolves_to_its_own_binding_not_to_a_same_named_accessor() {
        let src = "import { signal } from \"runai\"\n\
                   export default function P() { const n = signal(0); { const n = (v: number) => v; n(1) } return <p/> }\n";
        let b = bindings(src);
        assert!(b.accessor_calls.is_empty(), "{b:?}");
    }

    /// The mirror: the accessor's own read is recorded even though an inner block
    /// declares a same-named ordinary function.
    #[test]
    fn an_accessor_read_survives_a_same_named_binding_elsewhere() {
        let src = "import { signal } from \"runai\"\n\
                   export default function P() { const n = signal(0); { const n = (v: number) => v; n(1) } const x = n(); return <p/> }\n";
        let b = bindings(src);
        assert_eq!(b.accessor_calls.len(), 1, "{b:?}");
        assert!(!b.accessor_calls[0].write, "{b:?}");
    }

    /// An unimported `signal(…)` is not a primitive; `tsc` reports it later.
    #[test]
    fn an_unimported_primitive_is_not_a_primitive() {
        let b = bindings("const n = signal(0)\n");
        assert!(b.accessors.is_empty(), "{b:?}");
        assert!(b.primitive_calls.is_empty(), "{b:?}");
    }

    /// A primitive from another module is not this framework's primitive.
    #[test]
    fn a_primitive_from_a_foreign_module_is_not_a_primitive() {
        let b = bindings("import { signal } from \"./other\"\nconst n = signal(0)\n");
        assert!(b.accessors.is_empty(), "{b:?}");
    }

    /// A type-only import is stripped, so it cannot create a runtime accessor.
    #[test]
    fn a_type_only_import_is_not_a_primitive() {
        let b = bindings("import type { signal } from \"runai\"\nconst n = signal(0)\n");
        assert!(b.accessors.is_empty(), "{b:?}");
    }

    #[test]
    fn reads_and_writes_are_resolved_by_symbol() {
        let b = bindings(
            "import { signal } from \"runai\"\n\
             function P() { const n = signal(0); const x = n(); n(1); return <p/> }\n",
        );
        assert_eq!(b.accessor_calls.len(), 2, "{b:?}");
        assert!(!b.accessor_calls[0].write);
        assert!(b.accessor_calls[1].write);
        assert_eq!(b.accessor_calls[0].name, "n");
    }

    /// `signal(0)` is a primitive call, never a write to an accessor, even though it
    /// has one argument.
    #[test]
    fn a_primitive_call_is_not_an_accessor_write() {
        let b = bindings("import { signal } from \"runai\"\nconst n = signal(0)\n");
        assert!(b.accessor_calls.is_empty(), "{b:?}");
        assert_eq!(b.primitive_calls.len(), 1);
    }

    #[test]
    fn primitive_calls_include_every_primitive() {
        let b = bindings(
            "import { signal, derived, effect, resource } from \"runai\"\n\
             function P() { const n = signal(0); derived(() => n()); effect(() => {}); resource(() => 1); return <p/> }\n",
        );
        assert_eq!(b.primitive_calls.len(), 4, "{b:?}");
    }

    #[test]
    fn effect_scopes_are_the_function_arguments() {
        let b = bindings(
            "import { signal, derived, effect } from \"runai\"\n\
             function P() { const n = signal(0); effect(() => { n(1) }); derived(() => n()); return <p/> }\n",
        );
        assert_eq!(b.effect_scopes.len(), 1, "{b:?}");
    }

    /// A call with two arguments is neither the read nor the write this milestone
    /// defines, so it is not recorded as one.
    #[test]
    fn a_two_argument_call_is_not_a_read_or_a_write() {
        let b = bindings(
            "import { signal } from \"runai\"\nfunction P() { const n = signal(0); n(1, 2); return <p/> }\n",
        );
        assert!(b.accessor_calls.is_empty(), "{b:?}");
    }

    /// A primitive is a primitive *however it is called*: a second argument (which
    /// `effect`, `resource`, and `signal` all accept in the design) must not hide the
    /// call site from `primitive_calls`, which `RA010` will read as its source of
    /// truth.
    #[test]
    fn a_multi_argument_primitive_is_still_a_primitive_call() {
        let b = bindings(
            "import { signal, effect, resource } from \"runai\"\n\
             function P() { const n = signal(0, { equals: true }); effect(() => {}, { once: true }); resource(() => 1, { retries: 2 }); return <p/> }\n",
        );
        assert_eq!(b.primitive_calls.len(), 3, "{b:?}");
        assert_eq!(b.effect_scopes.len(), 1, "{b:?}");
    }

    /// [`SignalBindings::accessor_symbols`] is index-aligned with
    /// [`SignalBindings::accessors`]: entry `i` is *the* symbol of name `i`. The rules
    /// rely on this pairing (they record a call's symbol, then look the name up), and
    /// length alone does not prove it.
    #[test]
    fn accessor_symbols_are_the_symbols_of_the_accessors() {
        let src = "import { signal, derived } from \"runai\"\n\
                   const a = signal(0)\n\
                   function P() { const b = signal(1); const c = derived(() => a() + b()); return <p/> }\n";
        let b = bindings(src);
        assert_eq!(b.accessors.len(), 3, "{b:?}");
        assert_eq!(b.accessor_symbols.len(), b.accessors.len(), "{b:?}");

        // Resolve each recorded symbol back to its declared name with a second semantic
        // build: the names and the symbols have to agree position by position, not just
        // in count.
        let alloc = Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let semantic = oxc_semantic::SemanticBuilder::new().build(program).semantic;
        let scoping = semantic.scoping();
        for (index, (name, _)) in b.accessors.iter().enumerate() {
            assert_eq!(
                scoping.symbol_name(b.accessor_symbols[index]),
                name,
                "index {index} is misaligned"
            );
        }
    }

    /// The nearest function recorded for a call inside a component body is that
    /// component's body span, which is exactly the representation
    /// `ComponentInfo::body_span` uses.
    #[test]
    fn the_nearest_function_is_the_component_body_span() {
        let src = "import { signal } from \"runai\"\n\
                   export default function P() { const n = signal(0); const x = n(); return <p/> }\n";
        let b = bindings(src);
        let call = b
            .accessor_calls
            .iter()
            .find(|call| !call.write)
            .expect("a read");
        assert!(!call.in_jsx_container);
        let body = call.nearest_function.expect("inside a function");
        assert!(
            src[body.0 as usize..body.1 as usize].starts_with('{'),
            "{body:?}"
        );
    }

    #[test]
    fn a_read_in_a_jsx_slot_records_the_container() {
        let src = "import { signal } from \"runai\"\n\
                   export default function P() { const n = signal(0); return <p>{n()}</p> }\n";
        let b = bindings(src);
        assert!(
            b.accessor_calls.iter().any(|call| call.in_jsx_container),
            "{b:?}"
        );
    }

    /// An arrow nested in the JSX slot is the nearest function, and the container is
    /// *outside* it, so the call is not "in a JSX slot" relative to the handler.
    #[test]
    fn a_read_in_a_handler_is_inside_the_handler_not_the_slot() {
        let src = "import { signal } from \"runai\"\n\
                   export default function P() { const n = signal(0); return <button onClick={() => n()}>x</button> }\n";
        let b = bindings(src);
        let call = &b.accessor_calls[0];
        assert!(!call.in_jsx_container, "{b:?}");
    }
}
