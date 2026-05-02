package skills

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	agentskills "github.com/nijaru/agentskills"
)

type InstallPreview struct {
	Name         string
	Description  string
	AllowedTools []string
	Source       string
	Target       string
	Files        int
	Bytes        int64
}

func PreviewInstall(source, targetRoot string) (InstallPreview, error) {
	plan, err := planInstall(source, targetRoot)
	if err != nil {
		return InstallPreview{}, err
	}
	return plan.preview, nil
}

func Install(source, targetRoot string) (InstallPreview, error) {
	plan, err := planInstall(source, targetRoot)
	if err != nil {
		return InstallPreview{}, err
	}
	if err := os.MkdirAll(targetRoot, 0o755); err != nil {
		return InstallPreview{}, fmt.Errorf("create skills dir: %w", err)
	}
	stageRoot := filepath.Join(targetRoot, ".staging")
	if err := os.MkdirAll(stageRoot, 0o755); err != nil {
		return InstallPreview{}, fmt.Errorf("create skill staging dir: %w", err)
	}
	stageDir, err := os.MkdirTemp(stageRoot, plan.preview.Name+"-*")
	if err != nil {
		return InstallPreview{}, fmt.Errorf("create skill staging dir: %w", err)
	}
	cleanupStage := true
	defer func() {
		if cleanupStage {
			_ = os.RemoveAll(stageDir)
		}
	}()

	if err := copySkillBundle(plan.sourceDir, stageDir); err != nil {
		return InstallPreview{}, err
	}
	staged, err := agentskills.Load(filepath.Join(stageDir, "SKILL.md"))
	if err != nil {
		return InstallPreview{}, fmt.Errorf("validate staged skill: %w", err)
	}
	if staged.Name != plan.preview.Name {
		return InstallPreview{}, fmt.Errorf(
			"staged skill name changed from %q to %q",
			plan.preview.Name,
			staged.Name,
		)
	}
	if _, err := os.Stat(plan.preview.Target); err == nil {
		return InstallPreview{}, fmt.Errorf(
			"skill %q already exists at %s",
			plan.preview.Name,
			plan.preview.Target,
		)
	} else if !os.IsNotExist(err) {
		return InstallPreview{}, fmt.Errorf("check target skill dir: %w", err)
	}
	if err := os.Rename(stageDir, plan.preview.Target); err != nil {
		return InstallPreview{}, fmt.Errorf("install skill: %w", err)
	}
	cleanupStage = false
	return plan.preview, nil
}

type installPlan struct {
	sourceDir string
	preview   InstallPreview
}

func planInstall(source, targetRoot string) (installPlan, error) {
	sourceDir, skillPath, err := resolveSkillSource(source)
	if err != nil {
		return installPlan{}, err
	}
	skill, err := agentskills.Load(skillPath)
	if err != nil {
		return installPlan{}, fmt.Errorf("validate skill: %w", err)
	}
	targetRoot, err = filepath.Abs(expandHome(strings.TrimSpace(targetRoot)))
	if err != nil {
		return installPlan{}, fmt.Errorf("resolve skills dir: %w", err)
	}
	target := filepath.Join(targetRoot, skill.Name)
	if _, err := os.Stat(target); err == nil {
		return installPlan{}, fmt.Errorf("skill %q already exists at %s", skill.Name, target)
	} else if !os.IsNotExist(err) {
		return installPlan{}, fmt.Errorf("check target skill dir: %w", err)
	}
	files, bytes, err := scanSkillBundle(sourceDir)
	if err != nil {
		return installPlan{}, err
	}
	return installPlan{
		sourceDir: sourceDir,
		preview: InstallPreview{
			Name:         skill.Name,
			Description:  strings.TrimSpace(skill.Description),
			AllowedTools: append([]string(nil), []string(skill.AllowedTools)...),
			Source:       sourceDir,
			Target:       target,
			Files:        files,
			Bytes:        bytes,
		},
	}, nil
}

func resolveSkillSource(source string) (string, string, error) {
	source = strings.TrimSpace(source)
	if source == "" {
		return "", "", fmt.Errorf("skill source path is required")
	}
	if strings.Contains(source, "://") {
		return "", "", fmt.Errorf("remote skill sources are not supported yet")
	}
	path, err := filepath.Abs(expandHome(source))
	if err != nil {
		return "", "", fmt.Errorf("resolve skill source: %w", err)
	}
	info, err := os.Stat(path)
	if err != nil {
		return "", "", fmt.Errorf("read skill source: %w", err)
	}
	if info.IsDir() {
		return path, filepath.Join(path, "SKILL.md"), nil
	}
	if info.Mode().IsRegular() && info.Name() == "SKILL.md" {
		return filepath.Dir(path), path, nil
	}
	return "", "", fmt.Errorf("skill source must be a directory or SKILL.md file")
}

func scanSkillBundle(sourceDir string) (int, int64, error) {
	var files int
	var bytes int64
	err := filepath.WalkDir(sourceDir, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if path == sourceDir {
			return nil
		}
		if entry.IsDir() {
			if entry.Name() == ".git" {
				return filepath.SkipDir
			}
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			rel, _ := filepath.Rel(sourceDir, path)
			return fmt.Errorf("skill bundle contains unsupported non-regular file: %s", rel)
		}
		files++
		bytes += info.Size()
		return nil
	})
	if err != nil {
		return 0, 0, fmt.Errorf("scan skill bundle: %w", err)
	}
	return files, bytes, nil
}

func copySkillBundle(sourceDir, targetDir string) error {
	return filepath.WalkDir(sourceDir, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if path == sourceDir {
			return nil
		}
		rel, err := filepath.Rel(sourceDir, path)
		if err != nil {
			return err
		}
		if entry.IsDir() {
			if entry.Name() == ".git" {
				return filepath.SkipDir
			}
			if err := os.MkdirAll(filepath.Join(targetDir, rel), 0o755); err != nil {
				return fmt.Errorf("copy skill dir %s: %w", rel, err)
			}
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			return fmt.Errorf("skill bundle contains unsupported non-regular file: %s", rel)
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return fmt.Errorf("read skill file %s: %w", rel, err)
		}
		targetPath := filepath.Join(targetDir, rel)
		if err := os.MkdirAll(filepath.Dir(targetPath), 0o755); err != nil {
			return fmt.Errorf("create skill dir %s: %w", filepath.Dir(rel), err)
		}
		if err := os.WriteFile(targetPath, data, 0o644); err != nil {
			return fmt.Errorf("write skill file %s: %w", rel, err)
		}
		return nil
	})
}

func expandHome(path string) string {
	if path == "~" {
		if home, err := os.UserHomeDir(); err == nil {
			return home
		}
	}
	if strings.HasPrefix(path, "~/") {
		if home, err := os.UserHomeDir(); err == nil {
			return filepath.Join(home, path[2:])
		}
	}
	return path
}
