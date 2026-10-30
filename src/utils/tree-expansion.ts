import type { TreeNode } from "@/components/common/tree-view";

/** Keep existing branches and expand only newly requested branches. */
export const mergeExpandedIds = (
  current: Set<string>,
  nodes: TreeNode[],
  defaultIds: Set<string>,
  additionalIds: Set<string> = new Set()
): Set<string> => {
  const nodeIds = new Set<string>();
  const visit = (items: TreeNode[]) =>
    items.forEach((node) => {
      nodeIds.add(node.id);
      visit(node.children ?? []);
    });
  visit(nodes);
  const next = new Set<string>();
  current.forEach((id) => {
    if (nodeIds.has(id)) next.add(id);
  });
  defaultIds.forEach((id) => {
    if (!current.has(id)) next.add(id);
  });
  additionalIds.forEach((id) => next.add(id));
  return next;
};

export const branchIds = (nodes: TreeNode[]): Set<string> => {
  const result = new Set<string>();
  const visit = (items: TreeNode[]) =>
    items.forEach((node) => {
      if (node.children?.length) {
        result.add(node.id);
        visit(node.children);
      }
    });
  visit(nodes);
  return result;
};
