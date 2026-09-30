#!/bin/sh
# Leaves a descendant holding the inherited output handles after the leader
# reports its version, so the probe must not wait on those handles.
sleep 2 &
printf 'hog-devnode 0.1.12\n'
