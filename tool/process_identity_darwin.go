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
		return ProcessIdentity{}, fmt.Errorf("%w: pid %d is not its process-group leader", ErrProcessIdentityInvalid, pid)
	}
	return ProcessIdentity{
		Version:    processIdentityVersion,
		Platform:   "darwin",
		PID:        pid,
		PGID:       pgid,
		StartToken: fmt.Sprintf("%d:%d:%d:%d", boot.Sec, boot.Usec, proc.Proc.P_starttime.Sec, proc.Proc.P_starttime.Usec),
	}, nil
}

func (darwinProcessPlatform) terminateGroup(ctx context.Context, pgid int) error {
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
