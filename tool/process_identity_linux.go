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
	if fields[0] == "Z" || fields[0] == "X" {
		return ProcessIdentity{}, ErrProcessNotFound
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

func (p linuxProcessPlatform) terminateGroup(ctx context.Context, identity ProcessIdentity) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	observed, err := p.inspect(identity.PID)
	if err != nil {
		return err
	}
	if observed != identity {
		return ErrProcessIdentityChanged
	}
	// Linux can address the leader through a pidfd, which remains bound to the
	// original process across PID reuse. Kill the leader through that handle,
	// then signal the still-owned process group if it has descendants.
	fd, pidfdErr := unix.PidfdOpen(identity.PID, 0)
	if pidfdErr == nil {
		defer unix.Close(fd)
		if err := unix.PidfdSendSignal(fd, unix.SIGKILL, nil, 0); err != nil && !errors.Is(err, unix.ESRCH) {
			return err
		}
		exists, err := p.groupExists(identity.PGID)
		if err != nil {
			return err
		}
		if !exists {
			return nil
		}
	} else if !errors.Is(pidfdErr, unix.ENOSYS) && !errors.Is(pidfdErr, unix.EINVAL) {
		return fmt.Errorf("open process identity handle: %w", pidfdErr)
	}
	if err := unix.Kill(-identity.PGID, unix.SIGKILL); err != nil {
		if errors.Is(err, unix.ESRCH) {
			return ErrProcessNotFound
		}
		return err
	}
	return nil
}

func (linuxProcessPlatform) groupExists(pgid int) (bool, error) {
	entries, err := os.ReadDir("/proc")
	if err != nil {
		return false, err
	}
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		pid, err := strconv.Atoi(entry.Name())
		if err != nil || pid <= 0 {
			continue
		}
		data, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
		if err != nil {
			if errors.Is(err, os.ErrNotExist) {
				continue
			}
			return false, err
		}
		closeName := strings.LastIndex(string(data), ")")
		if closeName < 0 || closeName+2 > len(data) {
			continue
		}
		fields := strings.Fields(string(data[closeName+2:]))
		if len(fields) <= 2 || fields[0] == "Z" || fields[0] == "X" {
			continue
		}
		currentPGID, err := strconv.Atoi(fields[2])
		if err == nil && currentPGID == pgid {
			return true, nil
		}
	}
	return false, nil
}
