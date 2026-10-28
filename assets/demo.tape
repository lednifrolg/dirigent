# The README's demo, recorded with vhs (https://github.com/charmbracelet/vhs) against the demo
# hub in crates/dirigent/examples, so no request reaches a real hub. From the repository root:
#
#   cargo build -p dirigent --bin dirigent --examples && vhs assets/demo.tape

Output "assets/demo.gif"

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 14
Set Width 1200
Set Height 660
Set Padding 16
Set Theme "TokyoNight"
Set TypingSpeed 60ms
Set Framerate 30

Hide
Type "export DIRIGENT_HOME=$(mktemp -d) DIRIGENT_HUB_URL=http://127.0.0.1:18892 PATH=$PWD/target/debug:$PATH"
Enter
Type "target/debug/examples/demo_hub 18892 $DIRIGENT_HOME > /dev/null & sleep 1"
Enter
Type "PS1='\[\e[1;34m\]❯\[\e[0m\] '; clear"
Enter
Show

Type "dirigent list"
Enter
Sleep 2.5s
Type "dirigent dim ceiling 40"
Enter
Sleep 2.5s
Type "dirigent"
Enter
Sleep 2.5s

# The ceiling light: off, on, and dimmed.
Space
Sleep 1.2s
Space
Sleep 1.2s
Right
Sleep 300ms
Right
Sleep 1.8s

# The shelf strip: a preset color, then a hue of its own.
Down 3
Sleep 1s
Type "c"
Sleep 1.2s
Right
Sleep 700ms
Right
Sleep 700ms
Right
Sleep 900ms
Down
Sleep 400ms
Right 6
Sleep 1.2s
Enter
Sleep 1.5s

# A scene, which can be taken back for a while.
Down 4
Sleep 800ms
Enter
Sleep 3s
Type "u"
Sleep 2.5s

Type "?"
Sleep 3.5s
Type "?"
Sleep 600ms
Type "q"
Sleep 500ms

Hide
Type "kill %1"
Enter
