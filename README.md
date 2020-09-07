# appMarkable

This is a fairly dumb ui, meant to be a placeholder for apps who want to be started from [draft](https://github.com/dixonary/draft-reMarkable), [oxide](https://github.com/Eeems/oxide) or [remux](https://rmkit.dev/apps/remux).

## Usage

```
USAGE:
    appmarkable [OPTIONS] <command>... [args]...

ARGS:
    <command>...    Full path to the executable
    <args>...       Arguments for the executable

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -c, --custom-image <custom-image>    Display a custom full image instead of name and icon.
    -i, --icon <icon>                    Path for icon to display
        --icon-size <icon-size>          Size of icon to display (squared) [default: 500]
    -n, --name <name>                    App name to display
```

## Quick start

If you decided to make your app accessible as a package (ipkg) and wan't to have a place in above mentioned launchers, you can specify the same name and icon as used in the oxide file (full path though).

The following oxide file

```
name=myAwesomeApp
desc=This is my really cool app
call=/opt/bin/myawesomeapp
term=:
imgFile=myawesomeapp
```

would turn into

```
name=myAwesomeApp
desc=This is my really cool app
call=/opt/bin/appmarkable /opt/bin/myawesomeapp -n myAwesomeApp -i /opt/etc/draft/icons/myawesomeapp.png
term=:
imgFile=myawesomeapp
```

Also don't forget to make you package depend on appmarkable to make sure it is installed.

Now you have some UI that just signals that your app is running. The user can quit it by pressing the power and right button together (which sends a SIGINT just like CTRL+C would).

## Step it up a notch

If you want more control over the design, you can also just provide a full image to display instead with `--custom-image`. Though please tell the user how to quit the app, is it isn't done in this mode.