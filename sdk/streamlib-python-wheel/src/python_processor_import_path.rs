// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Deriving a processor class's import path — the string a helper process
//! imports it back by.
//!
//! Every Python processor runs in its own child interpreter, which reaches the
//! class by importing it and nothing else. So an identity that a fresh
//! interpreter cannot resolve is not a naming inconvenience — it is a class
//! with no host, and the only place that can be said usefully is `rt.add`.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

/// The module name CPython gives whatever file it was launched with. It names
/// a different file in the child (the child is launched with the helper
/// module), so it resolves to the wrong class or to nothing at all.
const ENTRY_FILE_MODULE: &str = "__main__";

/// CPython puts this in `__qualname__` for a class defined inside a function.
/// Nothing outside that call can name such a class, import included.
const FUNCTION_LOCAL_MARKER: &str = "<locals>";

/// A processor class's fully-qualified import path, `module:qualname`.
///
/// This is the string the registry names the type by, the control plane
/// reports, and the spawn host hands the child as `STREAMLIB_ENTRYPOINT`.
pub(crate) fn processor_class_import_path(processor_class: &Bound<'_, PyAny>) -> PyResult<String> {
    let module = processor_class
        .getattr("__module__")
        .and_then(|module| module.extract::<String>())
        .map_err(|_| {
            PyValueError::new_err(
                "a processor class must carry a `__module__` string; this one does not, so \
                 there is no import path to derive",
            )
        })?;
    let qualname = processor_class
        .getattr("__qualname__")
        .and_then(|qualname| qualname.extract::<String>())
        .map_err(|_| {
            PyValueError::new_err(
                "a processor class must carry a `__qualname__` string; this one does not, so \
                 there is no import path to derive",
            )
        })?;

    // Before the entry-file check: a class defined inside a function is
    // unimportable wherever it lives, and moving its module would not help.
    // Checking the module first would answer a class that is both with the
    // fix for the problem it does not have.
    if qualname.contains(FUNCTION_LOCAL_MARKER) {
        return Err(PyValueError::new_err(format!(
            "processor `{module}:{qualname}` is defined inside a function, so it identifies \
             as a name no interpreter can import — `{FUNCTION_LOCAL_MARKER}` marks a class \
             that exists only for the duration of a call. Every Python processor runs in its \
             own child process, which reaches the class by importing this name.\n\n\
             Move the class to module scope. If it was parameterised by the enclosing \
             function's arguments, pass those through `rt.add(..., config={{...}})` instead \
             — config reaches the child, a closure cannot."
        )));
    }

    if module == ENTRY_FILE_MODULE {
        return Err(PyValueError::new_err(format!(
            "processor `{qualname}` is defined in the entry file, so it identifies as \
             `__main__:{qualname}` — a name no other interpreter can import. Every Python \
             processor runs in its own child process, which imports the class by this name \
             and would get its own entry file instead.\n\n\
             Move `{qualname}` into an importable module beside the entry file and import it \
             from there — one import line:\n\n\
             \x20   # {module_suggestion}.py\n\
             \x20   @processor(...)\n\
             \x20   class {qualname}: ...\n\n\
             \x20   # app.py\n\
             \x20   from {module_suggestion} import {qualname}\n\n\
             The entry file itself may still run as `__main__`; only processor classes may \
             not live in it.",
            module_suggestion = suggested_module_name(&qualname),
        )));
    }

    Ok(format!("{module}:{qualname}"))
}

/// A plausible module name to put in the entry-file error's example, derived
/// from the class's own name so the suggestion reads like the user's code
/// rather than a placeholder.
fn suggested_module_name(qualname: &str) -> String {
    let leaf = qualname.rsplit('.').next().unwrap_or(qualname);
    let mut snake = String::with_capacity(leaf.len() + 4);
    for (position, character) in leaf.char_indices() {
        if character.is_ascii_uppercase() {
            if position != 0 {
                snake.push('_');
            }
            snake.push(character.to_ascii_lowercase());
        } else {
            snake.push(character);
        }
    }
    if snake.is_empty() {
        "processors".to_string()
    } else {
        snake
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::python_class_from_source_for_tests::class_from_source;

    #[test]
    fn a_module_scope_class_derives_module_colon_qualname() {
        Python::initialize();
        Python::attach(|python| {
            let class = class_from_source(
                python,
                "__name__ = 'my_app.filters'\nclass BlurProcessor: pass\n",
                "BlurProcessor",
            );
            assert_eq!(
                processor_class_import_path(&class).unwrap(),
                "my_app.filters:BlurProcessor"
            );
        });
    }

    /// A nested class is importable — `getattr` down the qualname resolves it —
    /// so the dotted qualname rides through rather than being refused.
    #[test]
    fn a_nested_class_keeps_its_dotted_qualname() {
        Python::initialize();
        Python::attach(|python| {
            let class = class_from_source(
                python,
                "__name__ = 'my_app.filters'\nclass Outer:\n    class Inner: pass\n",
                "Outer",
            );
            let inner = class.getattr("Inner").unwrap();
            assert_eq!(
                processor_class_import_path(&inner).unwrap(),
                "my_app.filters:Outer.Inner"
            );
        });
    }

    #[test]
    fn an_entry_file_class_is_refused_with_the_fix_named() {
        Python::initialize();
        Python::attach(|python| {
            let class = class_from_source(
                python,
                "__name__ = '__main__'\nclass BlurProcessor: pass\n",
                "BlurProcessor",
            );
            let refusal = processor_class_import_path(&class).unwrap_err();
            let message = refusal.to_string();
            assert!(
                message.contains("__main__:BlurProcessor"),
                "the refusal must show the identity that cannot be imported: {message}"
            );
            assert!(
                message.contains("importable module"),
                "the refusal must name the fix: {message}"
            );
            assert!(
                message.contains("blur_processor"),
                "the refusal's example must be derived from the class name: {message}"
            );
        });
    }

    #[test]
    fn a_function_local_class_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let class = class_from_source(
                python,
                "__name__ = 'my_app.filters'\n\
                 def build():\n    class BlurProcessor: pass\n    return BlurProcessor\n\
                 Built = build()\n",
                "Built",
            );
            let message = processor_class_import_path(&class).unwrap_err().to_string();
            assert!(
                message.contains("<locals>"),
                "the refusal must name what marks the class local: {message}"
            );
            assert!(
                message.contains("config="),
                "the refusal must name the way to pass what the closure captured: {message}"
            );
        });
    }

    /// A class can be both, and then only one of the two fixes is real: moving
    /// the module does nothing for a closure-local class. Mentally put the
    /// entry-file check first and this answers with the wrong fix — and with a
    /// suggested class name containing `<locals>`.
    #[test]
    fn a_function_local_class_in_the_entry_file_is_refused_for_being_local() {
        Python::initialize();
        Python::attach(|python| {
            let class = class_from_source(
                python,
                "__name__ = '__main__'\n\
                 def build():\n    class BlurProcessor: pass\n    return BlurProcessor\n\
                 Built = build()\n",
                "Built",
            );
            let message = processor_class_import_path(&class).unwrap_err().to_string();
            assert!(
                message.contains("config="),
                "a class that is both must be answered with the closure fix: {message}"
            );
            assert!(
                !message.contains("importable module"),
                "moving the module does not help a closure-local class: {message}"
            );
        });
    }

    #[test]
    fn the_suggested_module_name_is_snake_case() {
        assert_eq!(suggested_module_name("BlurProcessor"), "blur_processor");
        assert_eq!(suggested_module_name("Outer.Inner"), "inner");
        assert_eq!(suggested_module_name("HTTPSource"), "h_t_t_p_source");
    }
}
