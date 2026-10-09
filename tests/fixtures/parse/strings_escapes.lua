local s
s = "\a\b\f\n\r\t\v\\\"\'"
s = '\a\b\f\n\r\t\v\\\"\''
s = "\x41\x42\x43\x7a\x7F\xff\x00"
s = "\65\066\0677\0\00\000\255\1\12\123"
s = "a\z
      b\z     c"
s = "\u{48}\u{0}\u{7F}\u{80}\u{7FF}\u{800}\u{FFFF}\u{10000}\u{10FFFF}\u{000000041}"
s = "line1\
line2"
s = 'it\'s'
s = "tab	inside"
s = ""
s = ''
s = "\"quoted\""
s = '"single"'
s = "\\n"
s = "nul\0byte"
s = "high\200\201\254"
s = "\z"
s = "esc\27[0m"
s = "\u{E000}\u{D7FF}"
