# Chroma Engine Mediaway encoder patch

This directory vendors Mediaway 0.1.4's Windows encoder under its MIT license,
with the dependency and platform scope recorded in `Cargo.toml`.

The local patch releases every Media Foundation activation object returned during
encoder enumeration before freeing the COM-allocated array. Candidates after the
selected encoder and empty successful enumerations are also released.
