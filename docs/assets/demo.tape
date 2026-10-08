# Terminal demo for the README, recorded with charmbracelet/vhs:
#
#   vhs docs/assets/demo.tape        # writes docs/assets/demo.gif
#
# Run this on the rig, with a stream already running in another shell
# (`tatolab dev` from a scaffolded stream), so `nodes`, `graph` and `tap`
# have something live to answer about.

Output docs/assets/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1180
Set Height 640
Set Padding 22
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms

Type "tatolab nodes"
Enter
Sleep 2.5s

Type "tatolab graph | jq '.nodes[] | {name, state: .components.state}'"
Enter
Sleep 3.5s

Type "tatolab tap desk-my-rig-8kq3/camerasource/video --count 3"
Enter
Sleep 4s

Type "tatolab logs --follow --level warn"
Enter
Sleep 4s
Ctrl+C
Sleep 1.5s
