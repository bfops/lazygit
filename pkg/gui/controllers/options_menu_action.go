package controllers

import (
	"strings"

	"github.com/jesseduffield/lazygit/pkg/config"
	"github.com/jesseduffield/lazygit/pkg/gocui"
	"github.com/jesseduffield/lazygit/pkg/gui/types"
	"github.com/jesseduffield/lazygit/pkg/utils"
	"github.com/samber/lo"
)

type OptionsMenuAction struct {
	c *ControllerCommon
}

func (self *OptionsMenuAction) Call() error {
	ctx := self.c.Context().Current()
	sections := self.getBindings(ctx)

	menuItems := []*types.MenuItem{}

	appendBindings := func(bindings []*types.Binding, section *types.MenuSection) {
		menuItems = append(menuItems,
			lo.Map(bindings, func(binding *types.Binding, _ int) *types.MenuItem {
				var disabledReason *types.DisabledReason
				if binding.GetDisabledReason != nil {
					disabledReason = binding.GetDisabledReason()
				}
				tooltip := binding.Tooltip
				if len(binding.Keys) > 1 {
					if tooltip != "" {
						tooltip += "\n\n"
					}
					keyLabels := lo.Map(binding.Keys, func(k gocui.Key, _ int) string { return config.LabelForKey(k) })
					tooltip += self.c.Tr.KeybindingsTooltip + strings.Join(keyLabels, ", ")
				}
				return &types.MenuItem{
					OpensMenu: binding.OpensMenu,
					Label:     binding.GetDescription(),
					OnPress: func() error {
						if binding.Handler == nil {
							return nil
						}

						return self.c.IGuiCommon.CallKeybindingHandler(binding)
					},
					Keys:           binding.Keys,
					Tooltip:        tooltip,
					DisabledReason: disabledReason,
					Section:        section,
				}
			})...)
	}

	appendBindings(sections.review, &types.MenuSection{Title: "Review", Column: 1})
	appendBindings(sections.local, &types.MenuSection{Title: self.c.Tr.KeybindingsMenuSectionLocal, Column: 1})
	appendBindings(sections.global, &types.MenuSection{Title: self.c.Tr.KeybindingsMenuSectionGlobal, Column: 1})
	appendBindings(sections.navigation, &types.MenuSection{Title: self.c.Tr.KeybindingsMenuSectionNavigation, Column: 1})

	return self.c.Menu(types.CreateMenuOptions{
		Title:                      self.c.Tr.Keybindings,
		Items:                      menuItems,
		HideCancel:                 true,
		ColumnAlignment:            []utils.Alignment{utils.AlignRight, utils.AlignLeft},
		AllowFilteringKeybindings:  true,
		KeepConflictingKeybindings: true,
	})
}

type keybindingMenuSections struct {
	review     []*types.Binding
	local      []*types.Binding
	global     []*types.Binding
	navigation []*types.Binding
}

func (self *OptionsMenuAction) getBindings(context types.Context) keybindingMenuSections {
	var bindings []*types.Binding

	bindings, _ = self.c.GetInitialKeybindingsWithCustomCommands()
	return classifyKeybindingsForMenu(bindings, context.GetViewName(), self.c.Modes().Review.Active)
}

func classifyKeybindingsForMenu(bindings []*types.Binding, viewName string, reviewModeActive bool) keybindingMenuSections {
	result := keybindingMenuSections{}
	for _, binding := range bindings {
		if binding.GetDescription() != "" {
			if reviewModeActive && binding.Tag == "review" {
				result.review = append(result.review, binding)
			} else if binding.Tag == "review" && binding.ViewName != "" {
				continue
			} else if binding.ViewName == "" || binding.Tag == "global" {
				result.global = append(result.global, binding)
			} else if binding.ViewName == viewName {
				if binding.Tag == "navigation" {
					result.navigation = append(result.navigation, binding)
				} else {
					result.local = append(result.local, binding)
				}
			}
		}
	}

	result.review = uniqueBindings(result.review)
	result.local = uniqueBindings(result.local)
	result.global = uniqueBindings(result.global)
	result.navigation = uniqueBindings(result.navigation)
	return result
}

// We shouldn't really need to do this. We should define alternative keys for the same
// handler in the keybinding struct.
func uniqueBindings(bindings []*types.Binding) []*types.Binding {
	return lo.UniqBy(bindings, func(binding *types.Binding) string {
		return binding.GetDescription()
	})
}
