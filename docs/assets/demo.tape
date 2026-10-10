# Terminal demo for the README, recorded with charmbracelet/vhs:
#
#   vhs docs/assets/demo.tape        # writes docs/assets/demo.gif
#
# Run this on the rig, with `tatolabd` running in one shell and a stream
# loaded from another (`tatolab dev` from a scaffolded stream, loaded as
# `main`), so `streams`, `graph`, `tap` and `logs` have something live to
# answer about.

Output docs/assets/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1180
Set Height 640
Set Padding 22
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms

Type "tatolab streams"
Enter
Sleep 2.5s

Type "tatolab graph --stream main | jq '.nodes[] | {name, state: .components.state}'"
Enter
Sleep 3.5s

Type "tatolab tap desk-my-rig-8kq3/camerasource/video --stream main --count 3"
Enter
Sleep 4s

Type "tatolab logs --stream main --follow --level warn"
Enter
Sleep 4s
Ctrl+C
Sleep 1.5s
