// <Tabs syncKey="lang"><TabItem label="Python">…</TabItem>…</Tabs>, as in the
// MDX pages. The server reads the items; TabsClient switches them.
import { Children, isValidElement, type ReactElement, type ReactNode } from "react";
import TabsClient from "./TabsClient";

interface ItemProps {
  label: string;
  children?: ReactNode;
}

export function TabItem({ children }: ItemProps) {
  return <>{children}</>;
}

export function Tabs({ syncKey, children }: { syncKey?: string; children?: ReactNode }) {
  const items = Children.toArray(children).filter(
    (c): c is ReactElement<ItemProps> => isValidElement(c) && typeof (c.props as ItemProps).label === "string",
  );
  return (
    <TabsClient
      labels={items.map((i) => i.props.label)}
      panels={items.map((i) => i.props.children)}
      sync={syncKey === "lang"}
    />
  );
}
