{ lib }:
# Escape every byte outside lowercase ASCII/digits, including the escape marker
# itself. This is reversible and cannot alias two different account names.
username:
"u"
+ lib.concatMapStrings (
  c:
  if builtins.match "[a-z0-9]" c != null then
    c
  else
    "-${lib.toLower (lib.fixedWidthString 2 "0" (lib.toHexString (lib.strings.charToInt c)))}"
) (lib.stringToCharacters username)
