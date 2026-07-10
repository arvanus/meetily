"use client"

import * as React from "react"
import { ChevronLeft, ChevronRight } from "lucide-react"
import { DayPicker } from "react-day-picker"

import { cn } from "@/lib/utils"
import { buttonVariants } from "@/components/ui/button"

export type CalendarProps = React.ComponentProps<typeof DayPicker>

function Calendar({
  className,
  classNames,
  showOutsideDays = true,
  navLayout = "around",
  ...props
}: CalendarProps) {
  return (
    <DayPicker
      showOutsideDays={showOutsideDays}
      navLayout={navLayout}
      className={cn("p-3", className)}
      classNames={{
        months: "flex flex-col sm:flex-row gap-2",
        month: "relative flex flex-col gap-4",
        month_caption: "flex justify-center pt-1 items-center h-9",
        caption_label: "text-sm font-medium",
        button_previous: cn(
          buttonVariants({ variant: "outline" }),
          "absolute left-1 top-1 size-7 bg-transparent p-0 opacity-70 hover:opacity-100 z-10"
        ),
        button_next: cn(
          buttonVariants({ variant: "outline" }),
          "absolute right-1 top-1 size-7 bg-transparent p-0 opacity-70 hover:opacity-100 z-10"
        ),
        month_grid: "w-full border-collapse",
        weekdays: "flex",
        weekday: "text-muted-foreground rounded-md w-8 font-normal text-[0.8rem]",
        week: "flex w-full mt-1",
        day: "relative p-0 text-center text-sm focus-within:relative focus-within:z-20",
        day_button: cn(
          buttonVariants({ variant: "ghost" }),
          "size-8 p-0 font-normal aria-selected:opacity-100"
        ),
        range_start: "rounded-l-md bg-accent",
        range_middle: "rounded-none bg-accent/50",
        range_end: "rounded-r-md bg-accent",
        selected:
          "[&>button]:bg-primary [&>button]:text-primary-foreground [&>button]:hover:bg-primary [&>button]:hover:text-primary-foreground [&>button]:focus:bg-primary [&>button]:focus:text-primary-foreground",
        today: "[&>button]:bg-accent [&>button]:text-accent-foreground",
        outside:
          "day-outside text-muted-foreground aria-selected:text-muted-foreground",
        disabled: "text-muted-foreground opacity-50",
        hidden: "invisible",
        ...classNames,
      }}
      components={{
        PreviousMonthButton: ({ className, ...props }) => (
          <button className={className} {...props}>
            <ChevronLeft className="size-4" />
          </button>
        ),
        NextMonthButton: ({ className, ...props }) => (
          <button className={className} {...props}>
            <ChevronRight className="size-4" />
          </button>
        ),
      }}
      {...props}
    />
  )
}
Calendar.displayName = "Calendar"

export { Calendar }
