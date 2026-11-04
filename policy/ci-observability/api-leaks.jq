# The references a crate's rustdoc JSON makes, a signature's or a `pub use`'s,
# to an item rustdoc stripped, one line each: `just api-surface` fails on any.
# Given `$public` (each published crate's public paths and their last names,
# by crate name) and `$crates` (this JSON's external crate names, by id).
#
# A reference is stripped when its id is local but in neither `index` nor
# `paths`, or it is a published crate's item that crate's public paths lack.
# `index` also holds the external traits a crate references, by name only. An
# auto trait's synthetic impl is skipped: its bounds name private fields.
# rustdoc drops a `pub use` of an item itself hidden, which no check sees.
.index as $index | .paths as $paths
| def stripped: (.id | tostring) as $id
    | ($index[$id].crate_id // $paths[$id].crate_id) as $crate
    | if $crate == null then true
      elif $crate == 0 then false
      else $public[$crates[$crate | tostring] // ""] as $kept
        | $kept != null and (if $paths[$id] != null
            then $kept.paths[$paths[$id].path | join("::")]
            else $kept.names[$index[$id].name] end) == null
      end;
[$index[] | select(.crate_id == 0 and .inner.impl.is_synthetic != true) | . as $item
   | ((.. | objects | select(has("id") and has("path") and (.path | type) == "string"))
      , (.inner.use // empty | select(.id != null) | {id, path: .source}))
   | select(stripped)
   | "\($item.span.filename):\($item.span.begin[0]): \($item.name // $item.inner.use.name // "an impl") names \(.path)"]
| unique[]
