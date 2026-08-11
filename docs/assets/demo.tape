# Terminal demo for the README, recorded with charmbracelet/vhs:
#
#   vhs docs/assets/demo.tape        # writes docs/assets/demo.gif
#
# Run this on the rig, with a node already running in another shell
# (`streamlib dev` from a scaffolded app), so `nodes`, `graph` and `tap`
# have something live to answer about.

Output docs/assets/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1180
Set Height 640
Set Padding 22
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms

Type "streamlib nodes"
Enter
Sleep 2.5s

Type "streamlib graph | jq '.processors[] | {display_name, state}'"
Enter
Sleep 3.5s

Type "streamlib tap CameraSource/video --count 3"
Enter
Sleep 4s

Type "streamlib logs --follow --level warn"
Enter
Sleep 4s
Ctrl+C
Sleep 1.5s
