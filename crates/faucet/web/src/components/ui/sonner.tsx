import { Toaster as Sonner, type ToasterProps } from "sonner"

export function Toaster(props: ToasterProps) {
  return (
    <Sonner
      theme="dark"
      className="toaster group"
      toastOptions={{
        classNames: {
          toast:
            "group toast group-[.toaster]:bg-card group-[.toaster]:text-foreground group-[.toaster]:border-border group-[.toaster]:font-sans group-[.toaster]:shadow-none",
          description: "group-[.toast]:text-muted-foreground",
        },
      }}
      {...props}
    />
  )
}
