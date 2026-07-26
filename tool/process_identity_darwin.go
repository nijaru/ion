//go:build darwin

package tool

import (
	"context"
	"errors"
	"fmt"

	"golang.org/x/sys/unix"
)

type darwinProcessPlatform struct{}

func hostProcessPlatform() processPlatform { return darwinProcessPlatform{} }

func (darwinProcessPlatform) name() string { return "darwin" }

func (p darwinProcessPlatform) capture(pid int) (ProcessIdentity, error) {
	return p.inspect(pid)
}

func (darwinProcessPlatform) inspect(pid int) (ProcessIdentity, error) {
	proc, err := unix.SysctlKinfoProc("kern.proc.pid", pid)
	if err != nil {
		if errors.Is(err, unix.ESRCH) || errors.Is(err, unix.EIO) {
			return ProcessIdentity{}, ErrProcessNotFound
		}
		return ProcessIdentity{}, fmt.Errorf("inspect process: %w", err)
	}
	if int(proc.Proc.P_pid) != pid {
		return ProcessIdentity{}, ErrProcessNotFound
	}
	boot, err := unix.SysctlTimeval("kern.boottime")
	if err != nil {
		return ProcessIdentity{}, fmt.Errorf("read kernel boot identity: %w", err)
	}
	pgid := int(proc.Eproc.Pgid)
	if pgid <= 0 {
		return ProcessIdentity{}, fmt.Errorf("%w: invalid process group", ErrProcessIdentityInvalid)
	}
	actualPGID, err := unix.Getpgid(pid)
	if err != nil {
		if errors.Is(err, unix.ESRCH) || errors.Is(err, unix.EINVAL) {
			return ProcessIdentity{}, ErrProcessNotFound
		}
		return ProcessIdentity{}, fmt.Errorf("read process group: %w", err)
	}
	if actualPGID != pgid {
		return ProcessIdentity{}, fmt.Errorf("%w: process group changed during inspection", ErrProcessIdentityInvalid)
	}
	if pgid != pid {
		return ProcessIdentity{}, fmt.Errorf(
			"%w: pid %d is not its process-group leader",
			ErrProcessIdentityInvalid,
			pid,
		)
	}
	return ProcessIdentity{
		Version:  processIdentityVersion,
		Platform: "darwin",
		PID:      pid,
		PGID:     pgid,
		StartToken: fmt.Sprintf(
			"%d:%d:%d:%d",
			boot.Sec,
			boot.Usec,
			proc.Proc.P_starttime.Sec,
			proc.Proc.P_starttime.Usec,
		),
	}, nil
}

func (p darwinProcessPlatform) terminateGroup(ctx context.Context, identity ProcessIdentity) error {
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
	if err := unix.Kill(-identity.PGID, unix.SIGKILL); err != nil {
		if errors.Is(err, unix.ESRCH) {
			return ErrProcessNotFound
		}
		return err
	}
	return nil
}

func (darwinProcessPlatform) groupExists(pgid int) (bool, error) {
	processes, err := unix.SysctlKinfoProcSlice("kern.proc.pgrp", pgid)
	if err != nil {
		if errors.Is(err, unix.ESRCH) || errors.Is(err, unix.EIO) || errors.Is(err, unix.ENOENT) {
			return false, nil
		}
		return false, err
	}
	for _, process := range processes {
		// Darwin's SZOMB state is 5. Zombies no longer execute and do not
		// represent a live descendant that recovery must signal.
		if process.Proc.P_stat != 5 {
			return true, nil
		}
	}
	return false, nil
}
