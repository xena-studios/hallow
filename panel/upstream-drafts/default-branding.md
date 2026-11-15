# Issue: let deployments set the default application name and icon without a code change

**Kind:** feature request (issue first, per CONTRIBUTING.md)

## Draft

We run Calagopus as the panel of a Minecraft network and ship it under our own product name. Today the defaults for **Application Name** (`"Calagopus"`) and **Application Icon** (`/icon.svg`) are hard-coded in `shared/src/settings/app.rs` and `frontend/public/icon.svg`. An admin can change both in the settings, but a fresh install, the OOBE and any instance whose settings were never saved show the stock values. A distribution that wants its own name has to patch those two places.

Would you take a small change that reads the defaults from the environment when the setting has never been saved, for example:

```
APP_DEFAULT_NAME="Hallow Panel"
APP_DEFAULT_ICON="/branding/icon.svg"
```

with the current values as fallbacks, so nothing changes for anyone who doesn't set them? An extension could then ship the icon in its `public/` directory and the core files would stay untouched.

Two smaller spots that show "Calagopus" regardless of the configured name might be worth changing in the same pass, if you agree:

- `frontend/src/elements/AppIcon.tsx`: the `alt` text is `Calagopus Icon` / `Calagopus Banner` rather than `settings.app.name`.
- The OOBE welcome strings (`Welcome to Calagopus`, `Your Calagopus panel is ready to use`) could interpolate the application name.

We'd leave the footer credit (`Calagopus © 2025 – …`) as it is.

If this fits, one of us will write it by hand and open a PR.

## Notes for whoever sends it

- Matches `panel/HALLOW-CHANGES.md` entries 1 and 2. If upstream ships it, drop both core edits from the `hallow` branch and set the variables in our deployment instead.
- Ask in their Discord first if the issue tracker is quiet.
