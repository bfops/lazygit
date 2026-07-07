package controllers

import (
	"testing"

	"github.com/jesseduffield/lazygit/pkg/gui/types"
	"github.com/stretchr/testify/assert"
)

func TestClassifyKeybindingsForMenuMovesReviewBindingsIntoReviewSection(t *testing.T) {
	bindings := []*types.Binding{
		{ViewName: "", Description: "Toggle PR review mode", Tag: "review"},
		{ViewName: "main", Description: "Next review hunk", Tag: "review"},
		{ViewName: "main", Description: "Scroll down"},
		{ViewName: "", Description: "Quit"},
		{ViewName: "main", Description: "Navigate", Tag: "navigation"},
	}

	sections := classifyKeybindingsForMenu(bindings, "main", true)

	assert.Equal(t, []string{"Toggle PR review mode", "Next review hunk"}, bindingDescriptions(sections.review))
	assert.Equal(t, []string{"Scroll down"}, bindingDescriptions(sections.local))
	assert.Equal(t, []string{"Quit"}, bindingDescriptions(sections.global))
	assert.Equal(t, []string{"Navigate"}, bindingDescriptions(sections.navigation))
}

func TestClassifyKeybindingsForMenuLeavesReviewBindingsGlobalWhenReviewInactive(t *testing.T) {
	bindings := []*types.Binding{
		{ViewName: "", Description: "Toggle PR review mode", Tag: "review"},
		{ViewName: "main", Description: "Next review hunk", Tag: "review"},
	}

	sections := classifyKeybindingsForMenu(bindings, "main", false)

	assert.Empty(t, sections.review)
	assert.Empty(t, sections.local)
	assert.Equal(t, []string{"Toggle PR review mode"}, bindingDescriptions(sections.global))
}

func bindingDescriptions(bindings []*types.Binding) []string {
	descriptions := make([]string, len(bindings))
	for i, binding := range bindings {
		descriptions[i] = binding.GetDescription()
	}
	return descriptions
}
