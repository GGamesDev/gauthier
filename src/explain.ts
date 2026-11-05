// Compiler errors explained in plain words (like Thonny's Assistant).
// Keyed by rustc error code or lint name.

export interface Explanation {
  what: string;
  fix: string;
}

const E: Record<string, Explanation> = {
  E0308: {
    what: 'A value does not have the type Rust expects here (for example text where a number is needed).',
    fix: 'Check the type written after `:` or what the function returns. To turn text into a number use `.parse()`; to turn a number into text use `.to_string()` or `format!`.',
  },
  E0382: {
    what: 'The value was moved somewhere else, so this variable cannot be used any more.',
    fix: 'Lend it instead of moving it: pass `&value` (or `&mut value`). If you really need two copies, use `.clone()`.',
  },
  E0384: {
    what: 'You change a variable that is not mutable. In Rust variables cannot change unless you say so.',
    fix: 'Declare it with `let mut` instead of `let`.',
  },
  E0596: {
    what: 'You try to modify something through a reference or variable that does not allow changes.',
    fix: 'Declare the variable with `let mut`, or take a mutable reference `&mut`.',
  },
  E0425: {
    what: 'This name is unknown here: a variable or function that does not exist (or not at this place).',
    fix: 'Check the spelling, and that the variable is declared before, in the same block `{ }` or an enclosing one.',
  },
  E0433: {
    what: 'Rust cannot find this module or type path.',
    fix: 'Check the spelling and add the missing `use …;` at the top of the file. External crates must be added with the Crates button.',
  },
  E0432: {
    what: 'A `use` line refers to something that does not exist.',
    fix: 'Check the path in the `use` line. For an external crate, add it with the Crates button.',
  },
  E0412: {
    what: 'This type name is unknown.',
    fix: 'Check the spelling (types start with a capital letter) or add the `use` line that brings it in.',
  },
  E0599: {
    what: 'This method or field does not exist for this type.',
    fix: 'Check the spelling, or hover the value to see its type. Some methods need a `use` line for their trait.',
  },
  E0609: {
    what: 'This struct has no field with this name.',
    fix: 'Check the field names in the struct definition.',
  },
  E0061: {
    what: 'The function is called with the wrong number of arguments.',
    fix: 'Look at the function definition (hover its name) and pass exactly the parameters it asks for.',
  },
  E0502: {
    what: 'You borrow the same value for reading and for writing at the same time, which Rust forbids.',
    fix: 'Finish using the first borrow before the second one, for example by storing what you need in a new variable first.',
  },
  E0499: {
    what: 'The same value is borrowed mutably twice at the same time.',
    fix: 'Use one `&mut` at a time: end the first use before starting the second.',
  },
  E0506: {
    what: 'You assign to a value while it is still borrowed.',
    fix: 'Make sure the borrow is no longer used before changing the value.',
  },
  E0505: {
    what: 'You move a value while it is still borrowed.',
    fix: 'Use the borrow first, or clone the value before moving it.',
  },
  E0597: {
    what: 'A reference outlives the value it points to (the value is dropped at the end of its block).',
    fix: 'Declare the value in an outer block so it lives long enough, or return/store an owned value instead of a reference.',
  },
  E0106: {
    what: 'A reference type needs a lifetime here.',
    fix: 'Often simpler for beginners: return or store an owned value (`String` instead of `&str`).',
  },
  E0277: {
    what: 'This type cannot do what is asked (it does not implement the needed trait), for example printing with `{}` or adding two different types.',
    fix: 'For printing a struct use `{:?}` and add `#[derive(Debug)]` above it. For maths, convert both values to the same type with `as`.',
  },
  E0369: {
    what: 'This operator (+, -, ==, <…) cannot be used with these types.',
    fix: 'Convert the values to the same type (`as f64`, `as i32`), or derive/implement the trait (`#[derive(PartialEq)]` for `==`).',
  },
  E0282: {
    what: 'Rust cannot guess the type of this value.',
    fix: 'Write the type yourself, for example `let n: i32 = …` or `.parse::<i32>()`.',
  },
  E0423: {
    what: 'A type or module is used as if it were a value.',
    fix: 'To create a struct write `Name { field: value }`; to call a function add `()`.',
  },
  E0614: {
    what: 'You dereference (`*`) something that is not a reference.',
    fix: 'Remove the `*`.',
  },
  E0004: {
    what: 'The `match` does not handle every possible case.',
    fix: 'Add the missing cases, or a final `_ => …` arm for "everything else".',
  },
  E0023: {
    what: 'The pattern has the wrong number of fields for this enum variant.',
    fix: 'Match exactly as many values as the variant holds.',
  },
  E0063: {
    what: 'Some fields are missing when creating this struct.',
    fix: 'Give a value to every field listed in the struct definition.',
  },
  E0560: {
    what: 'This struct has no field with this name.',
    fix: 'Check the field names in the struct definition.',
  },
  E0603: {
    what: 'This item is private: it cannot be used from here.',
    fix: 'Add `pub` before its definition, or use a public alternative.',
  },
  E0716: {
    what: 'A temporary value is dropped while a reference to it is still used.',
    fix: 'Store the value in a variable with `let` first, then borrow that variable.',
  },
  E0015: {
    what: 'Only simple constant values can be computed in a `const` or `static`.',
    fix: 'Compute the value inside a function instead.',
  },
  // Common warnings
  unused_variables: {
    what: 'This variable is never used.',
    fix: 'Remove it, use it, or start its name with `_` to say it is unused on purpose.',
  },
  unused_mut: {
    what: 'This variable is declared `mut` but never changed.',
    fix: 'Remove `mut`.',
  },
  unused_imports: {
    what: 'This `use` line brings in something that is never used.',
    fix: 'Remove the line.',
  },
  dead_code: {
    what: 'This function, struct or field is never used.',
    fix: 'Remove it, or call/use it somewhere.',
  },
  unused_assignments: {
    what: 'A value is stored in a variable but overwritten before being read.',
    fix: 'Remove the first assignment, or use the value before changing it.',
  },
  unreachable_code: {
    what: 'This code can never run (it comes after a `return`, `break` or an endless `loop`).',
    fix: 'Remove it or move it before the line that leaves.',
  },
  unused_must_use: {
    what: 'The result of this call is ignored, but it may contain an error.',
    fix: 'Handle it with `?`, `.unwrap()`, or `let _ = …;` to ignore it on purpose.',
  },
  non_snake_case: {
    what: 'Rust names variables and functions in snake_case (lowercase with `_`).',
    fix: 'Rename it, for example `myValue` → `my_value`.',
  },
};

export function explain(code: string | null | undefined, message: string): Explanation | null {
  if (code && E[code]?.what) return E[code];
  // A few useful cases without a code.
  if (/cannot find macro `println`/.test(message)) {
    return { what: 'On the Pico, `println!` comes from picorust_runtime.', fix: 'Add `use picorust_runtime::println;` at the top of the file.' };
  }
  if (/expected `;`/.test(message)) return { what: 'A semicolon is missing at the end of the previous statement.', fix: 'Add `;` at the end of that line.' };
  if (/unclosed delimiter|mismatched closing delimiter/.test(message)) {
    return { what: 'A bracket `(`, `[` or `{` is not closed (or closed by the wrong one).', fix: 'Count the brackets: every opening one needs its matching closing one.' };
  }
  return null;
}
