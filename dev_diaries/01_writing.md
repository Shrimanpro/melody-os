# Colors

The first thing that I decided to do is make an easy way to print text out. I got Hello World to work, so it was essentially making that into a function. Now it's better to explain how a vga buffer works before I explain what I did. A vga buffer lives at a certain memory address and it's in-charge of making text appear on the screen. A quick google search can tell you that this memory address is at 0xb8000. From this address, a certain amount of bytes are reserved for displaying content on the screen.

In my Hello World code, I added two bytes for each character in the string "Hello World". This is because the first byte is for the actual character. The second one, however, is actually for the color. For testing purposes, I used light cyan, or 0xb. 

Now if I actually wanted an easy to print stuff, I would need to first make a way for the colors to be easily accessible. Hence, the enum, and then the struct. The struct is for using 2 of the color enums, because each byte representing a color, actually represents the foreground and background color.


# Writing

So after making a struct that does pretty much everything I described above, I had to make a way to automate this. Instead of calling or setting every single field in the struct, I could just call a function to do everything. Normally, println! would do this, but since we don't really have a stack or RAM to put the original function in, we can't use that. Ultimately the end goal would be to make said println! function usable, but for now we have to take baby steps.

I made a function that automatically writes a character. From my knowledge of my other project, the rest was pretty simple. Of course, I had to check if that char was going off the screen, or it was a new line char. Then I called that function multiple times in a new function that now takes in Strings. I am yet to test it out though (will definitely work!).
