//go:build linux

package tool

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"

	"golang.org/x/sys/unix"
)

type linuxProcessPlatform struct{}

func hostProcessPlatform() processPlatform { return linuxProcessPlatform{} }

func (linuxProcessPlatform) name() string { return "linux" }

func (p linuxProcessPlatform) capture(pid int) (ProcessIdentity, error) {
	return p.inspect(pid)
}

func (linuxProcessPlatform) inspect(pid int) (ProcessIdentity, error) {
	data, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return ProcessIdentity{}, ErrProcessNotFound
		}
		return ProcessIdentity{}, fmt.Errorf("read process stat: %w", err)
	}
	closeName := strings.LastIndex(string(data), ")")
	if closeName < 0 || closeName+2 > len(data) {
		return ProcessIdentity{}, fmt.Errorf("%w: malformed /proc stat", ErrProcessIdentityInvalid)
	}
	fields := strings.Fields(string(data[closeName+2:]))
	// The slice starts at field 3 (state): field 5/pgrp is index 2 and
	// field 22/starttime is index 19.
	if len(fields) <= 19 {
		return ProcessIdentity{}, fmt.Errorf("%w: incomplete /proc stat", ErrProcessIdentityInvalid)
	}
	pgid, err := strconv.Atoi(fields[2])
	if err != nil {
		return ProcessIdentity{}, fmt.Errorf("%w: invalid process group: %v", ErrProcessIdentityInvalid, err)
	}
	startTicks, err := strconv.ParseUint(fields[19], 10, 64)
	if err != nil {
		return ProcessIdentity{}, fmt.Errorf("%w: invalid process start time: %v", ErrProcessIdentityInvalid, err)
	}
	bootID, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil {
		return ProcessIdentity{}, fmt.Errorf("read kernel boot identity: %w", err)
	}
	boot := strings.TrimSpace(string(bootID))
	if boot == "" {
		return ProcessIdentity{}, fmt.Errorf("%w: empty kernel boot identity", ErrProcessIdentityInvalid)
	}
	actualPGID, err := unix.Getpgid(pid)
	if err != nil {
		if errors.Is(err, unix.ESRCH) {
			return ProcessIdentity{}, ErrProcessNotFound
		}
		return ProcessIdentity{}, fmt.Errorf("read process group: %w", err)
	}
	if actualPGID != pgid {
		return ProcessIdentity{}, fmt.Errorf("%w: process group changed during inspection", ErrProcessIdentityInvalid)
	}
	if pgid != pid {
		return ProcessIdentity{}, fmt.Errorf("%w: pid %d is not its process-group leader", ErrProcessIdentityInvalid, pid)
	}
	return ProcessIdentity{
		Version:    processIdentityVersion,
		Platform:   "linux",
		PID:        pid,
		PGID:       pgid,
		StartToken: fmt.Sprintf("%s:%d", boot, startTicks),
	}, nil
}

func (linuxProcessPlatform) terminateGroup(ctx context.Context, pgid int) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := unix.Kill(-pgid, unix.SIGKILL); err != nil {
		if errors.Is(err, unix.ESRCH) {
			return ErrProcessNotFound
		}
		return err
	}
	return nil
}
